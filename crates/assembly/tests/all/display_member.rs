//! Tests for the F# member-signature pretty-printer
//! ([`borzoi_assembly::format_member`]). A curated pin table: each case
//! builds a `Member` + owning `Entity` and asserts the exact signature line.

use borzoi_assembly::{
    Access, AssemblyIdentity, Augmentation, ConstantValue, Entity, EntityKind, Event, Field,
    IndexParameter, Member, MethodLike, MethodSignature, ModuleValue, Nullability, NullableType,
    ParamDefault, Parameter, Primitive, Property, TypeParameter, TypeRef, Variance, Version,
    format_entity_header, format_member,
};

// ---- construction helpers -------------------------------------------------

fn assembly_id() -> AssemblyIdentity {
    AssemblyIdentity {
        name: "Asm".to_string(),
        version: Version {
            major: 1,
            minor: 0,
            build: 0,
            revision: 0,
        },
        public_key_token: None,
    }
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

/// A fully-defaulted entity; tests override the few fields `format_member`
/// reads (`kind`, `generic_parameters`, `name`/`source_name`).
fn entity(name: &str, kind: EntityKind, typars: &[&str]) -> Entity {
    Entity {
        extension_member_names: Vec::new(),
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        assembly: assembly_id(),
        namespace: vec![],
        name: name.to_string(),
        kind,
        access: Access::Public,
        generic_parameters: typars.iter().map(|t| typar(t)).collect(),
        base_type: None,
        interfaces: vec![],
        members: vec![],
        skipped_members: vec![],
        method_def_tokens: vec![],
        is_sealed: false,
        nested_types: vec![],
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: false,
        is_require_qualified_access: false,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        obsolete: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: vec![],
        source_name: None,
        custom_attrs: vec![],
        abbreviation_target: None,
    }
}

fn class(name: &str) -> Entity {
    entity(name, EntityKind::Class, &[])
}

fn prim(p: Primitive) -> TypeRef {
    TypeRef::Primitive(p)
}

fn type_var(index: u16) -> TypeRef {
    TypeRef::Var {
        index,
        is_method: false,
    }
}

fn method_var(index: u16) -> TypeRef {
    TypeRef::Var {
        index,
        is_method: true,
    }
}

fn named0(ns: &[&str], name: &str) -> TypeRef {
    TypeRef::Named {
        assembly: None,
        namespace: ns.iter().map(|s| s.to_string()).collect(),
        name: name.to_string(),
        type_args: vec![],
        segment_arities: vec![0],
    }
}

fn fsharp_func(from: TypeRef, to: TypeRef) -> TypeRef {
    TypeRef::Named {
        assembly: None,
        namespace: vec![
            "Microsoft".to_string(),
            "FSharp".to_string(),
            "Core".to_string(),
        ],
        name: "FSharpFunc".to_string(),
        type_args: vec![NullableType::oblivious(from), NullableType::oblivious(to)],
        segment_arities: vec![2],
    }
}

fn fsharp_option(inner: TypeRef) -> TypeRef {
    TypeRef::Named {
        assembly: None,
        namespace: vec![
            "Microsoft".to_string(),
            "FSharp".to_string(),
            "Core".to_string(),
        ],
        name: "FSharpOption".to_string(),
        type_args: vec![NullableType::oblivious(inner)],
        segment_arities: vec![1],
    }
}

fn param(name: Option<&str>, ty: TypeRef) -> Parameter {
    Parameter {
        name: name.map(|s| s.to_string()),
        ty,
        is_byref: false,
        is_out: false,
        is_readonly_ref: false,
        default: ParamDefault::None,
        is_param_array: false,
        nullability: Nullability::Oblivious,
    }
}

fn sig(params: Vec<Parameter>, ret: TypeRef) -> MethodSignature {
    MethodSignature {
        parameters: params,
        return_type: ret,
        return_nullability: Nullability::Oblivious,
    }
}

fn method(name: &str, signature: MethodSignature) -> MethodLike {
    MethodLike {
        name: name.to_string(),
        access: Access::Public,
        signature,
        arg_group_count: Some(1),
        is_static: false,
        is_virtual: false,
        is_abstract: false,
        is_constructor: false,
        module_value: None,
        is_module_value_binding: false,
        is_extension_method: false,
        augmentation: Augmentation::No,
        is_final: false,
        is_newslot: false,
        is_hide_by_sig: false,
        generic_parameters: vec![],
        obsolete: None,
        experimental: None,
        sets_required_members: false,
        compiler_feature_required: vec![],
        source_name: None,
        custom_attrs: vec![],
        metadata_token: 0,
        implements: Vec::new(),
        unclassified_impls: Vec::new(),
    }
}

fn field(name: &str, ty: TypeRef) -> Field {
    Field {
        name: name.to_string(),
        access: Access::Public,
        ty,
        is_static: false,
        is_init_only: false,
        is_volatile: false,
        is_literal: false,
        is_required: false,
        compiler_feature_required: vec![],
        nullability: Nullability::Oblivious,
        custom_attrs: vec![],
    }
}

fn property(name: &str, ty: TypeRef, has_getter: bool, has_setter: bool) -> Property {
    Property {
        name: name.to_string(),
        access: Access::Public,
        ty,
        parameters: vec![],
        is_static: false,
        has_getter,
        has_setter,
        getter_access: has_getter.then_some(Access::Public),
        is_required: false,
        compiler_feature_required: vec![],
        nullability: Nullability::Oblivious,
        custom_attrs: vec![],
        implements: Vec::new(),
        unclassified_impls: Vec::new(),
    }
}

fn event(name: &str, delegate_type: TypeRef) -> Event {
    Event {
        name: name.to_string(),
        access: Access::Public,
        delegate_type,
        is_static: false,
        has_fire: false,
        nullability: Nullability::Oblivious,
        custom_attrs: vec![],
        implements: Vec::new(),
        unclassified_impls: Vec::new(),
    }
}

// ---- methods --------------------------------------------------------------

#[test]
fn static_method() {
    let mut m = method(
        "WriteLine",
        sig(
            vec![param(Some("value"), prim(Primitive::String))],
            prim(Primitive::Void),
        ),
    );
    m.is_static = true;
    assert_eq!(
        format_member(&Member::Method(m), &class("Console")),
        "static member WriteLine: value: string -> unit"
    );
}

#[test]
fn instance_method_uses_owner_typar() {
    // `member Add: item: 'T -> unit` on `List<'T>`.
    let m = method(
        "Add",
        sig(
            vec![param(Some("item"), type_var(0))],
            prim(Primitive::Void),
        ),
    );
    assert_eq!(
        format_member(
            &Member::Method(m),
            &entity("List", EntityKind::Class, &["T"])
        ),
        "member Add: item: 'T -> unit"
    );
}

#[test]
fn zero_arg_method_takes_unit() {
    let m = method("Hash", sig(vec![], prim(Primitive::I4)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Thing")),
        "member Hash: unit -> int"
    );
}

#[test]
fn multi_arg_method_tuples_params() {
    let m = method(
        "Bar",
        sig(
            vec![
                param(Some("a"), prim(Primitive::I4)),
                param(Some("b"), prim(Primitive::String)),
            ],
            prim(Primitive::Bool),
        ),
    );
    assert_eq!(
        format_member(&Member::Method(m), &class("Thing")),
        "member Bar: a: int * b: string -> bool"
    );
}

#[test]
fn unnamed_param_renders_type_only() {
    let m = method(
        "Foo",
        sig(
            vec![param(None, prim(Primitive::String))],
            prim(Primitive::Void),
        ),
    );
    assert_eq!(
        format_member(&Member::Method(m), &class("Thing")),
        "member Foo: string -> unit"
    );
}

#[test]
fn generic_method_lists_method_typars() {
    // `member Id<'a>: x: 'a -> 'a`.
    let mut m = method(
        "Id",
        sig(vec![param(Some("x"), method_var(0))], method_var(0)),
    );
    m.generic_parameters = vec![typar("a")];
    assert_eq!(
        format_member(&Member::Method(m), &class("Thing")),
        "member Id<'a>: x: 'a -> 'a"
    );
}

#[test]
fn abstract_method() {
    let mut m = method(
        "Compare",
        sig(
            vec![param(Some("other"), prim(Primitive::I4))],
            prim(Primitive::I4),
        ),
    );
    m.is_abstract = true;
    assert_eq!(
        format_member(
            &Member::Method(m),
            &entity("IComparable", EntityKind::Interface, &[])
        ),
        "abstract member Compare: other: int -> int"
    );
}

#[test]
fn compiled_name_uses_source_name() {
    // `printfn`, not the compiled `PrintFormatLine`.
    let mut m = method(
        "PrintFormatLine",
        sig(
            vec![param(Some("format"), prim(Primitive::String))],
            prim(Primitive::Void),
        ),
    );
    m.source_name = Some("printfn".to_string());
    assert_eq!(
        format_member(&Member::Method(m), &class("Printf")),
        "member printfn: format: string -> unit"
    );
}

#[test]
fn constructor_returns_owner_type() {
    let mut m = method(
        ".ctor",
        sig(
            vec![param(Some("value"), prim(Primitive::String))],
            prim(Primitive::Void),
        ),
    );
    m.is_constructor = true;
    assert_eq!(
        format_member(&Member::Method(m), &class("Container")),
        "new: value: string -> Container"
    );
}

#[test]
fn parameterless_constructor_takes_unit_and_generic_owner() {
    let mut m = method(".ctor", sig(vec![], prim(Primitive::Void)));
    m.is_constructor = true;
    assert_eq!(
        format_member(
            &Member::Method(m),
            &entity("Box", EntityKind::Class, &["T"])
        ),
        "new: unit -> Box<'T>"
    );
}

#[test]
fn module_function_renders_as_val() {
    let m = method(
        "add",
        sig(
            vec![
                param(Some("a"), prim(Primitive::I4)),
                param(Some("b"), prim(Primitive::I4)),
            ],
            prim(Primitive::I4),
        ),
    );
    assert_eq!(
        format_member(&Member::Method(m), &entity("Math", EntityKind::Module, &[])),
        "val add: a: int * b: int -> int"
    );
}

#[test]
fn byref_param_renders_byref() {
    // The projector strips `TypeSig::ByRef` into `is_byref`, leaving the
    // referent in `ty`; the signature must re-wrap it as `byref<…>`.
    let mut value = param(Some("value"), type_var(1)); // 'TValue
    value.is_byref = true;
    let m = method(
        "TryGetValue",
        sig(
            vec![param(Some("key"), type_var(0)), value],
            prim(Primitive::Bool),
        ),
    );
    assert_eq!(
        format_member(
            &Member::Method(m),
            &entity("Dictionary", EntityKind::Class, &["TKey", "TValue"])
        ),
        "member TryGetValue: key: 'TKey * value: byref<'TValue> -> bool"
    );
}

#[test]
fn readonly_byref_param_renders_inref() {
    // An `in` / `ref readonly` parameter is a *read-only* byref — F#'s
    // `inref<'T>`. It is a byref like any other (`is_byref`), so only
    // `is_readonly_ref` distinguishes the rendering from `byref<…>`; getting it
    // wrong would tell the reader they may write through a reference they may
    // not.
    let mut value = param(Some("value"), prim(Primitive::I4));
    value.is_byref = true;
    value.is_readonly_ref = true;
    let m = method("Sum", sig(vec![value], prim(Primitive::I4)));
    assert_eq!(
        format_member(&Member::Method(m), &entity("Host", EntityKind::Class, &[])),
        "member Sum: value: inref<int> -> int"
    );
}

#[test]
fn readonly_byref_return_renders_inref() {
    // `ref readonly int Pick()` — the byref lives in the return *type*, so the
    // read-only bit rides `TypeRef::ByRef { readonly }` rather than a parameter
    // flag.
    let m = method(
        "Pick",
        sig(
            vec![],
            TypeRef::ByRef {
                inner: Box::new(prim(Primitive::I4)),
                readonly: true,
            },
        ),
    );
    assert_eq!(
        format_member(&Member::Method(m), &entity("Host", EntityKind::Class, &[])),
        "member Pick: unit -> inref<int>"
    );
}

#[test]
fn out_param_renders_outref() {
    let mut result = param(Some("result"), prim(Primitive::I4));
    result.is_byref = true; // an `out` param is byref + [Out] in IL
    result.is_out = true;
    let m = method(
        "TryParse",
        sig(
            vec![param(Some("s"), prim(Primitive::String)), result],
            prim(Primitive::Bool),
        ),
    );
    assert_eq!(
        format_member(&Member::Method(m), &class("Int32")),
        "member TryParse: s: string * result: outref<int> -> bool"
    );
}

#[test]
fn function_valued_param_is_parenthesized() {
    // A callback parameter must read as `(int -> string)`, not collapse into the
    // method's own arrow.
    let mapping = param(
        Some("mapping"),
        fsharp_func(prim(Primitive::I4), prim(Primitive::String)),
    );
    let m = method("Apply", sig(vec![mapping], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Thing")),
        "member Apply: mapping: (int -> string) -> unit"
    );
}

#[test]
fn static_abstract_method() {
    // A static abstract interface member (IWSAM / generic math) keeps both
    // modifiers, not just `static`.
    let mut m = method(
        "op_Addition",
        sig(
            vec![param(Some("x"), type_var(0)), param(Some("y"), type_var(0))],
            type_var(0),
        ),
    );
    m.is_static = true;
    m.is_abstract = true;
    assert_eq!(
        format_member(
            &Member::Method(m),
            &entity("IAdditionOperators", EntityKind::Interface, &["T"])
        ),
        "static abstract member op_Addition: x: 'T * y: 'T -> 'T"
    );
}

#[test]
fn module_value_method_renders_as_val() {
    // A module-level `let` value is rebranded from its property getter to a
    // 0-parameter method, marked `module_value`; render it as the value it is
    // (`val pi: float`), not a callable `val pi: unit -> float`.
    let mut m = method("pi", sig(vec![], prim(Primitive::R8)));
    m.module_value = Some(ModuleValue { is_mutable: false });
    assert_eq!(
        format_member(&Member::Method(m), &entity("Math", EntityKind::Module, &[])),
        "val pi: float"
    );
}

#[test]
fn mutable_module_value_method_renders_mutable() {
    // `let mutable counter = 0`: the rebranded getter carries `is_mutable`.
    let mut m = method("counter", sig(vec![], prim(Primitive::I4)));
    m.module_value = Some(ModuleValue { is_mutable: true });
    assert_eq!(
        format_member(
            &Member::Method(m),
            &entity("State", EntityKind::Module, &[])
        ),
        "val mutable counter: int"
    );
}

#[test]
fn module_unit_function_renders_with_arrow() {
    // `let ping () = 1` is a genuine 0-parameter method (not a rebranded value,
    // so `module_value` is `None`); it is a `unit -> int` function — no longer
    // collapsed to a value by the old 0-parameter heuristic.
    let m = method("ping", sig(vec![], prim(Primitive::I4)));
    assert_eq!(
        format_member(
            &Member::Method(m),
            &entity("Hello", EntityKind::Module, &[])
        ),
        "val ping: unit -> int"
    );
}

#[test]
fn generic_module_value_renders_as_val() {
    // `let empty<'T> = …` is a *generic* value: F# emits it as a 0-parameter
    // generic *method* (not a property), so the property→method rebrand never
    // tags it `module_value`. It must still render as a value (`val empty<'T>:
    // 'T[]`), not a unit function (`val empty<'T>: unit -> 'T[]`) — regression
    // guard for FSharp.Core values like `Array.empty`/`List.empty`.
    let array_of_t = TypeRef::Array {
        element: Box::new(NullableType::oblivious(method_var(0))),
        rank: 1,
        sizes: vec![],
        lower_bounds: vec![],
    };
    let mut m = method("empty", sig(vec![], array_of_t));
    m.generic_parameters = vec![typar("T")];
    assert_eq!(
        format_member(
            &Member::Method(m),
            &entity("Array", EntityKind::Module, &[])
        ),
        "val empty<'T>: 'T[]"
    );
}

#[test]
fn fsharp_optional_param() {
    // `?count: int` — an `[<OptionalArgument>]` parameter, typed `FSharpOption<int>`,
    // unwrapped to its inner type with the F# `?` marker.
    let mut count = param(Some("count"), fsharp_option(prim(Primitive::I4)));
    count.default = ParamDefault::FSharpOptional;
    let m = method("Print", sig(vec![count], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Logger")),
        "member Print: ?count: int -> unit"
    );
}

#[test]
fn dotnet_optional_param_without_value() {
    // A value-less `[Optional]` / COM optional → `[<Optional>]`, distinct from
    // the F# `?` form.
    let mut x = param(Some("x"), prim(Primitive::I4));
    x.default = ParamDefault::Optional(None);
    let m = method("Take", sig(vec![x], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Counter")),
        "member Take: [<Optional>] x: int -> unit"
    );
}

#[test]
fn dotnet_default_value_param_renders_value() {
    // A C# default value renders inline (`name: T = <value>`), dropping the
    // `[<Optional>]` marker.
    let mut count = param(Some("count"), prim(Primitive::I4));
    count.default = ParamDefault::Optional(Some(ConstantValue::Int(16)));
    let mut sep = param(Some("sep"), prim(Primitive::String));
    sep.default = ParamDefault::Optional(Some(ConstantValue::String(",".to_string())));
    let m = method("Join", sig(vec![count, sep], prim(Primitive::String)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Strings")),
        "member Join: count: int = 16 * sep: string = \",\" -> string"
    );
}

#[test]
fn decimal_default_renders_with_m_suffix() {
    // `[DecimalConstant]` → the F# decimal literal: mantissa with the point at
    // `scale`, the `M` suffix. `1.5m` is mantissa 15, scale 1.
    let mut d = param(Some("d"), named0(&["System"], "Decimal"));
    d.default = ParamDefault::Optional(Some(ConstantValue::Decimal {
        negative: false,
        scale: 1,
        mantissa: 15,
    }));
    let m = method("Scale", sig(vec![d], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Money")),
        "member Scale: d: decimal = 1.5M -> unit"
    );
}

#[test]
fn decimal_default_renders_sign_padding_and_scale() {
    // A negative value, a sub-unit value needing a leading `0.0…`, and a
    // scale-0 integer all render exactly (no float).
    let cases = [
        (
            ConstantValue::Decimal {
                negative: true,
                scale: 1,
                mantissa: 25,
            },
            "-2.5M",
        ),
        (
            ConstantValue::Decimal {
                negative: false,
                scale: 3,
                mantissa: 5,
            },
            "0.005M",
        ),
        (
            ConstantValue::Decimal {
                negative: false,
                scale: 0,
                mantissa: 42,
            },
            "42M",
        ),
        (
            // Trailing zeros are preserved: `1.50m` (scale 2) stays distinct.
            ConstantValue::Decimal {
                negative: false,
                scale: 2,
                mantissa: 150,
            },
            "1.50M",
        ),
    ];
    for (value, expected) in cases {
        let mut d = param(Some("d"), named0(&["System"], "Decimal"));
        d.default = ParamDefault::Optional(Some(value));
        let m = method("M", sig(vec![d], prim(Primitive::Void)));
        assert_eq!(
            format_member(&Member::Method(m), &class("Money")),
            format!("member M: d: decimal = {expected} -> unit")
        );
    }
}

#[test]
fn datetime_default_renders_as_constructor_call() {
    // No F# `DateTime` literal exists; render the ctor call F# accepts.
    let mut t = param(Some("t"), named0(&["System"], "DateTime"));
    t.default = ParamDefault::Optional(Some(ConstantValue::DateTime(630822816000000000)));
    let m = method("At", sig(vec![t], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Clock")),
        "member At: t: DateTime = System.DateTime(630822816000000000L) -> unit"
    );
}

// ---- nullability (`T?`) at member positions --------------------------------

#[test]
fn nullable_return_renders_question_mark() {
    // `member Find: key: string -> string?` — the return annotation surfaces.
    let mut m = method(
        "Find",
        sig(
            vec![param(Some("key"), prim(Primitive::String))],
            prim(Primitive::String),
        ),
    );
    m.signature.return_nullability = Nullability::Annotated;
    assert_eq!(
        format_member(&Member::Method(m), &class("Cache")),
        "member Find: key: string -> string?"
    );
}

#[test]
fn nullable_parameter_renders_question_mark() {
    // `member Put: value: string? -> unit` — the parameter annotation surfaces.
    let mut value = param(Some("value"), prim(Primitive::String));
    value.nullability = Nullability::Annotated;
    let m = method("Put", sig(vec![value], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Cache")),
        "member Put: value: string? -> unit"
    );
}

#[test]
fn nullable_field_renders_question_mark() {
    let mut f = field("cache", prim(Primitive::String));
    f.nullability = Nullability::Annotated;
    assert_eq!(
        format_member(&Member::Field(f), &class("Box")),
        "val mutable cache: string?"
    );
}

#[test]
fn nullable_property_renders_question_mark() {
    let mut p = property("Name", prim(Primitive::String), true, false);
    p.nullability = Nullability::Annotated;
    assert_eq!(
        format_member(&Member::Property(p), &class("Person")),
        "member Name: string? with get"
    );
}

#[test]
fn nullable_index_parameter_renders_question_mark() {
    // `string this[string? key]` — the index parameter's annotation surfaces.
    let mut p = property("Item", prim(Primitive::String), true, false);
    p.parameters = vec![IndexParameter {
        name: Some("key".to_string()),
        ty: NullableType {
            ty: prim(Primitive::String),
            nullability: Nullability::Annotated,
        },
        is_param_array: false,
    }];
    assert_eq!(
        format_member(&Member::Property(p), &class("Lookup")),
        "member Item: key: string? -> string with get"
    );
}

#[test]
fn nullable_byref_return_keeps_question_mark_inside() {
    // A byref return whose referent is nullable renders `byref<string?>`, not
    // `byref<string>?` — the annotation is the referent's.
    let mut m = method(
        "GetRef",
        sig(
            vec![],
            TypeRef::ByRef {
                inner: Box::new(prim(Primitive::String)),
                readonly: false,
            },
        ),
    );
    m.signature.return_nullability = Nullability::Annotated;
    assert_eq!(
        format_member(&Member::Method(m), &class("Slot")),
        "member GetRef: unit -> byref<string?>"
    );
}

#[test]
fn fsharp_optional_param_keeps_inner_nullability() {
    // `?name: string?` — the `FSharpOption`'s annotated argument keeps its `?`
    // when the option is unwrapped.
    let mut name = param(
        Some("name"),
        TypeRef::Named {
            assembly: None,
            namespace: vec![
                "Microsoft".to_string(),
                "FSharp".to_string(),
                "Core".to_string(),
            ],
            name: "FSharpOption".to_string(),
            type_args: vec![NullableType {
                ty: prim(Primitive::String),
                nullability: Nullability::Annotated,
            }],
            segment_arities: vec![1],
        },
    );
    name.default = ParamDefault::FSharpOptional;
    let m = method("Greet", sig(vec![name], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Greeter")),
        "member Greet: ?name: string? -> unit"
    );
}

// ---- params arrays (`[<ParamArray>]`) --------------------------------------

/// A rank-1 array of `element`, the shape a `params T[]` parameter carries.
fn array_of(element: TypeRef) -> TypeRef {
    TypeRef::Array {
        element: Box::new(NullableType::oblivious(element)),
        rank: 1,
        sizes: vec![],
        lower_bounds: vec![],
    }
}

#[test]
fn param_array_renders_attribute_prefix() {
    // `int Sum(params int[] values)` → `[<ParamArray>] values: int[]`. F# writes
    // the params array as a parameter attribute (there is no `params` keyword),
    // so the attribute prefix — like `[<Optional>]` — is the faithful signature,
    // and it attaches to the *specific* variadic parameter, not the whole member.
    let mut values = param(Some("values"), array_of(prim(Primitive::I4)));
    values.is_param_array = true;
    let m = method("Sum", sig(vec![values], prim(Primitive::I4)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Counter")),
        "member Sum: [<ParamArray>] values: int[] -> int"
    );
}

#[test]
fn param_array_prefix_marks_only_the_trailing_param() {
    // `string Format(string fmt, params object[] args)` — only the trailing
    // parameter is variadic, so only it carries the marker.
    let mut args = param(Some("args"), array_of(named0(&["System"], "Object")));
    args.is_param_array = true;
    let m = method(
        "Format",
        sig(
            vec![param(Some("fmt"), prim(Primitive::String)), args],
            prim(Primitive::String),
        ),
    );
    assert_eq!(
        format_member(&Member::Method(m), &class("Strings")),
        "member Format: fmt: string * [<ParamArray>] args: obj[] -> string"
    );
}

#[test]
fn nameless_param_array_renders_attribute_prefix() {
    // Stripped metadata: no parameter name, so the marker precedes the bare type.
    let mut values = param(None, array_of(prim(Primitive::I4)));
    values.is_param_array = true;
    let m = method("Sum", sig(vec![values], prim(Primitive::I4)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Counter")),
        "member Sum: [<ParamArray>] int[] -> int"
    );
}

// `[<ParamArray>]` is orthogonal to the optional/default forms: F# lets a params
// array also be optional, and the projector then sets `is_param_array` *with* a
// non-`None` `ParamDefault` (verified with `dotnet fsi`: `[<ParamArray; Optional>]`
// and `[<ParamArray; OptionalArgument>]` both compile and emit both markers, and
// FCS renders `[<ParamArray>] ?xs: 'T[]`). The marker must survive every arm, so
// each `ParamDefault` variant gets a case here.

#[test]
fn param_array_with_fsharp_optional_keeps_both_markers() {
    // `[<ParamArray; OptionalArgument>] xs: obj[] option` → `[<ParamArray>] ?xs: obj[]`.
    let mut xs = param(
        Some("xs"),
        fsharp_option(array_of(named0(&["System"], "Object"))),
    );
    xs.default = ParamDefault::FSharpOptional;
    xs.is_param_array = true;
    let m = method("M", sig(vec![xs], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Host")),
        "member M: [<ParamArray>] ?xs: obj[] -> unit"
    );
}

#[test]
fn param_array_with_dotnet_optional_keeps_both_markers() {
    // `[<ParamArray; Optional>] xs: obj[]` → both attribute groups; the params
    // marker is not swallowed by the `[<Optional>]` arm.
    let mut xs = param(Some("xs"), array_of(named0(&["System"], "Object")));
    xs.default = ParamDefault::Optional(None);
    xs.is_param_array = true;
    let m = method("M", sig(vec![xs], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Host")),
        "member M: [<ParamArray>] [<Optional>] xs: obj[] -> unit"
    );
}

#[test]
fn param_array_with_default_value_keeps_marker() {
    // `[<ParamArray; Optional; DefaultParameterValue(null)>] xs: obj[]` — the
    // params marker precedes the inline default value.
    let mut xs = param(Some("xs"), array_of(named0(&["System"], "Object")));
    xs.default = ParamDefault::Optional(Some(ConstantValue::Null));
    xs.is_param_array = true;
    let m = method("M", sig(vec![xs], prim(Primitive::Void)));
    assert_eq!(
        format_member(&Member::Method(m), &class("Host")),
        "member M: [<ParamArray>] xs: obj[] = null -> unit"
    );
}

#[test]
fn param_array_index_parameter_renders_attribute_prefix() {
    // `member _.Item with get ([<ParamArray>] xs: int[])` is valid F# (verified
    // with `dotnet fsi`; FCS renders `member Item: [<ParamArray>] xs: int[] -> int
    // with get`). The marker rides the property/indexer path, which threads a
    // `Parameter` through `IndexParameter`.
    let mut p = property("Item", prim(Primitive::I4), true, false);
    p.parameters = vec![IndexParameter {
        name: Some("xs".to_string()),
        ty: NullableType::oblivious(array_of(prim(Primitive::I4))),
        is_param_array: true,
    }];
    assert_eq!(
        format_member(&Member::Property(p), &class("Lookup")),
        "member Item: [<ParamArray>] xs: int[] -> int with get"
    );
}

// ---- fields ---------------------------------------------------------------

#[test]
fn mutable_field() {
    let f = field("count", prim(Primitive::I4));
    assert_eq!(
        format_member(&Member::Field(f), &class("Counter")),
        "val mutable count: int"
    );
}

#[test]
fn static_field_carries_static() {
    let mut f = field("Empty", named0(&["System"], "String"));
    f.is_static = true;
    f.is_init_only = true;
    assert_eq!(
        format_member(&Member::Field(f), &class("String")),
        "static val Empty: string"
    );
}

#[test]
fn literal_field_is_marked_and_not_mutable() {
    // A `[<Literal>]` const / enum case uses the CLR `Literal` flag, not
    // `initonly`, so `is_init_only` is false — but it is a constant: marked
    // `[<Literal>]` and never `mutable`.
    let mut f = field("MaxValue", prim(Primitive::I4));
    f.is_static = true;
    f.is_init_only = false;
    f.is_literal = true;
    assert_eq!(
        format_member(&Member::Field(f), &class("Int32")),
        "[<Literal>] static val MaxValue: int"
    );
}

#[test]
fn volatile_field_is_marked() {
    // A C# `volatile` field is F#'s `[<VolatileField>]`. Without the marker the
    // hover would be indistinguishable from an ordinary mutable field — the very
    // memory-model distinction the projector keeps the `modreq(IsVolatile)` for.
    let mut f = field("Counter", prim(Primitive::I4));
    f.is_volatile = true;
    assert_eq!(
        format_member(&Member::Field(f), &class("VolatileHost")),
        "[<VolatileField>] val mutable Counter: int"
    );

    // …and a `static volatile` one keeps both.
    let mut f = field("Ready", prim(Primitive::Bool));
    f.is_volatile = true;
    f.is_static = true;
    assert_eq!(
        format_member(&Member::Field(f), &class("VolatileHost")),
        "[<VolatileField>] static val mutable Ready: bool"
    );
}

#[test]
fn static_mutable_field_is_mutable() {
    // A genuine static mutable field (not init-only, not a literal) now renders
    // `mutable` — the case the old `!is_static` heuristic could not express.
    let mut f = field("counter", prim(Primitive::I4));
    f.is_static = true;
    assert_eq!(
        format_member(&Member::Field(f), &class("Stats")),
        "static val mutable counter: int"
    );
}

#[test]
fn readonly_field() {
    let mut f = field("name", prim(Primitive::String));
    f.is_init_only = true;
    assert_eq!(
        format_member(&Member::Field(f), &class("Person")),
        "val name: string"
    );
}

// ---- properties -----------------------------------------------------------

#[test]
fn read_only_property() {
    let p = property("Count", prim(Primitive::I4), true, false);
    assert_eq!(
        format_member(&Member::Property(p), &class("List")),
        "member Count: int with get"
    );
}

#[test]
fn read_write_property() {
    let p = property("Capacity", prim(Primitive::I4), true, true);
    assert_eq!(
        format_member(&Member::Property(p), &class("List")),
        "member Capacity: int with get, set"
    );
}

#[test]
fn write_only_property() {
    let p = property("Password", prim(Primitive::String), false, true);
    assert_eq!(
        format_member(&Member::Property(p), &class("Login")),
        "member Password: string with set"
    );
}

#[test]
fn static_property() {
    let mut p = property("Now", named0(&["System"], "DateTime"), true, false);
    p.is_static = true;
    assert_eq!(
        format_member(&Member::Property(p), &class("DateTime")),
        "static member Now: DateTime with get"
    );
}

/// An indexer (`this[i]`) renders its index dimension before the element type,
/// like a method's parameters: `member Item: i: int -> 'T`.
fn index_param(name: Option<&str>, ty: TypeRef) -> IndexParameter {
    IndexParameter {
        name: name.map(|s| s.to_string()),
        ty: NullableType::oblivious(ty),
        is_param_array: false,
    }
}

#[test]
fn indexer_renders_index_parameter() {
    // `'T this[int i] { get; set; }` on `List<'T>` → the named index dimension.
    let mut p = property("Item", type_var(0), true, true);
    p.parameters = vec![index_param(Some("i"), prim(Primitive::I4))];
    assert_eq!(
        format_member(
            &Member::Property(p),
            &entity("List", EntityKind::Class, &["T"])
        ),
        "member Item: i: int -> 'T with get, set"
    );
}

#[test]
fn multi_index_indexer_tuples_with_star() {
    // `int this[int x, int y] { get; }` → the two indices tuple with `*`.
    let mut p = property("Item", prim(Primitive::I4), true, false);
    p.parameters = vec![
        index_param(Some("x"), prim(Primitive::I4)),
        index_param(Some("y"), prim(Primitive::I4)),
    ];
    assert_eq!(
        format_member(&Member::Property(p), &class("Grid")),
        "member Item: x: int * y: int -> int with get"
    );
}

#[test]
fn nameless_index_parameter_renders_type_only() {
    // Stripped metadata: the accessor carries no parameter name, so the index
    // renders as the bare type (no `name:`).
    let mut p = property("Item", prim(Primitive::String), true, false);
    p.parameters = vec![index_param(None, prim(Primitive::I4))];
    assert_eq!(
        format_member(&Member::Property(p), &class("Lookup")),
        "member Item: int -> string with get"
    );
}

#[test]
fn module_value_renders_as_val() {
    // An F# module-level `let` value compiles to a get-only property; hover
    // should read it back as `val`, not `member … with get`.
    let p = property("pi", prim(Primitive::R8), true, false);
    assert_eq!(
        format_member(
            &Member::Property(p),
            &entity("Math", EntityKind::Module, &[])
        ),
        "val pi: float"
    );
}

#[test]
fn mutable_module_value_renders_as_val_mutable() {
    let p = property("counter", prim(Primitive::I4), true, true);
    assert_eq!(
        format_member(
            &Member::Property(p),
            &entity("State", EntityKind::Module, &[])
        ),
        "val mutable counter: int"
    );
}

// ---- events ---------------------------------------------------------------

#[test]
fn event_marks_cli_event() {
    let e = event("Click", named0(&["System"], "EventHandler"));
    assert_eq!(
        format_member(&Member::Event(e), &class("Button")),
        "[<CLIEvent>] member Click: EventHandler"
    );
}

#[test]
fn static_event() {
    let mut e = event("Reset", named0(&["System"], "EventHandler"));
    e.is_static = true;
    assert_eq!(
        format_member(&Member::Event(e), &class("Counter")),
        "[<CLIEvent>] static member Reset: EventHandler"
    );
}

// ---- entity headers -------------------------------------------------------

#[test]
fn entity_header_class_and_typars() {
    assert_eq!(
        format_entity_header(&entity("List", EntityKind::Class, &["T"])),
        "type List<'T>"
    );
}

#[test]
fn entity_header_module() {
    assert_eq!(
        format_entity_header(&entity("Operators", EntityKind::Module, &[])),
        "module Operators"
    );
}

#[test]
fn entity_header_exception() {
    assert_eq!(
        format_entity_header(&entity("MyError", EntityKind::Exception, &[])),
        "exception MyError"
    );
}

#[test]
fn entity_header_struct_gets_struct_attr() {
    let mut e = entity("Vector2", EntityKind::Struct, &[]);
    e.is_struct = true;
    assert_eq!(format_entity_header(&e), "[<Struct>] type Vector2");
}

#[test]
fn entity_header_struct_record() {
    let mut e = entity("Point", EntityKind::Record, &["T"]);
    e.is_struct = true;
    assert_eq!(format_entity_header(&e), "[<Struct>] type Point<'T>");
}

#[test]
fn entity_header_enum_omits_struct_attr() {
    // Enums are value types, but `[<Struct>]` is not valid on them.
    let mut e = entity("Color", EntityKind::Enum, &[]);
    e.is_struct = true;
    assert_eq!(format_entity_header(&e), "type Color");
}

#[test]
fn entity_header_measure() {
    assert_eq!(
        format_entity_header(&entity("kg", EntityKind::Measure, &[])),
        "[<Measure>] type kg"
    );
}

#[test]
fn entity_header_auto_open_module() {
    let mut e = entity("ExtraTopLevelOperators", EntityKind::Module, &[]);
    e.is_auto_open = true;
    assert_eq!(
        format_entity_header(&e),
        "[<AutoOpen>] module ExtraTopLevelOperators"
    );
}

#[test]
fn entity_header_rqa_union() {
    let mut e = entity("Color", EntityKind::Union, &[]);
    e.is_require_qualified_access = true;
    assert_eq!(
        format_entity_header(&e),
        "[<RequireQualifiedAccess>] type Color"
    );
}

#[test]
fn entity_header_readonly_byref_struct_attr_order() {
    let mut e = entity("Span", EntityKind::Struct, &["T"]);
    e.is_struct = true;
    e.is_readonly = true;
    e.is_byref_like = true;
    assert_eq!(
        format_entity_header(&e),
        "[<Struct; IsReadOnly; IsByRefLike>] type Span<'T>"
    );
}

#[test]
fn entity_header_uses_source_name() {
    let mut e = entity("FSharpValueOption", EntityKind::Union, &["T"]);
    e.source_name = Some("ValueOption".to_string());
    e.is_struct = true;
    assert_eq!(format_entity_header(&e), "[<Struct>] type ValueOption<'T>");
}
