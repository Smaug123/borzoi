//! Human-facing F# pretty-printer for the entity model.
//!
//! Renders a [`TypeRef`] (and, in later slices, members and entity headers)
//! as an F#-flavoured type expression suitable for LSP hover. This is a
//! *separate* renderer from the differential normaliser in the
//! `test_support` module: that one emits CLR-shaped, fully-qualified strings
//! for byte-faithful comparison against fcs-dump, whereas this one emits
//! short, sugared F# (`int list`, `string -> bool`, `byref<'T>`) for a human
//! to read. The two deliberately do not share a code path.
//!
//! The conventions mirror G-Research's `ApiSurface/Type.fs` pretty-printer:
//! short names (namespace stripped), F# primitive aliases, postfix generics
//! for a single type argument (`int list`), angle brackets for two or more
//! (`Map<int, string>`), `FSharpFunc` rendered as arrows, and `System.Tuple`
//! / `System.ValueTuple` rendered as `(a * b)` / `struct (a * b)` (flattening
//! the CLR `TRest` chain so an 8+ element tuple stays one flat tuple).
//!
//! A nested generic type places each argument on its declaring segment using
//! the model's per-segment arity (`Dictionary<int, string>.Enumerator`, not
//! `Dictionary.Enumerator<int, string>`); see [`TypeRef::Named`]'s
//! `segment_arities`.
//!
//! # Deliberate approximations
//!
//! This is a readable pretty-printer, not a faithful round-trip of the model.
//! One place drops detail *on purpose* (documented here so the omission is
//! explicit, never silent):
//!
//! - **Bounded arrays.** `TypeRef::Array` `sizes` / `lower_bounds` (hand-authored
//!   IL such as `T[2..5, *]`, which no F#/C# compiler emits) are not surfaced;
//!   such an array renders by rank alone (`T[,]`), like an ordinary array.
//!
//! Nullable-reference annotations *are* surfaced: an `Annotated` position
//! ([`NullableType`] inner positions, or a `Parameter`/`Field`/… outer position
//! threaded in via [`format_nullable_type`]) renders the C#-style postfix `?`
//! (`string?`, `(int list)?`). `NotAnnotated`/`Oblivious` render plain — the `!`
//! the differential normaliser uses for `NotAnnotated` would be hover noise. The
//! root [`format_type`] entry has no outer annotation (it is not on the
//! [`TypeRef`]), so it renders inner `?`s only.

use crate::model::{
    ConstantValue, Entity, EntityKind, Event, Field, IndexParameter, Member, MethodLike,
    Nullability, NullableType, ParamDefault, Parameter, Primitive, Property, TypeParameter,
    TypeRef,
};

/// The generic parameters in scope when rendering a type, so that
/// [`TypeRef::Var`] resolves to a name (`'T`) rather than a positional
/// placeholder. `type_typars` are the enclosing entity's; `method_typars`
/// the enclosing method's (empty outside a method).
pub struct TyparScope<'a> {
    pub type_typars: &'a [TypeParameter],
    pub method_typars: &'a [TypeParameter],
}

impl<'a> TyparScope<'a> {
    /// Build a scope from the type- and method-level typar lists.
    pub fn new(type_typars: &'a [TypeParameter], method_typars: &'a [TypeParameter]) -> Self {
        Self {
            type_typars,
            method_typars,
        }
    }

    /// A scope with no typars in scope. A [`TypeRef::Var`] under this scope
    /// renders as a visible placeholder.
    pub fn empty() -> TyparScope<'static> {
        TyparScope {
            type_typars: &[],
            method_typars: &[],
        }
    }

    /// Name of the typar at `index` in the relevant list, prefixed with `'`.
    /// An out-of-range index (a projector bug, not a legal program) renders
    /// as a visible `'?<n>` placeholder rather than panicking or guessing —
    /// the renderer never aborts and never invents a wrong name.
    fn var_name(&self, index: u16, is_method: bool) -> String {
        let list = if is_method {
            self.method_typars
        } else {
            self.type_typars
        };
        match list.get(index as usize) {
            Some(tp) => format!("'{}", tp.name),
            None => format!("'?{}{}", if is_method { "M" } else { "" }, index),
        }
    }
}

/// Render a type as an F# type expression. The outermost position's
/// nullable-reference annotation lives on the enclosing structural field
/// (`Parameter`/`Field`/…), not the [`TypeRef`], so this entry renders the type
/// *without* an outer `?`; inner positions ([`NullableType`] in generic args /
/// array elements) still surface their own `?`. Use [`format_nullable_type`]
/// when the outer annotation is available.
pub fn format_type(ty: &TypeRef, scope: &TyparScope) -> String {
    render(ty, scope).0
}

/// Render a type together with its outermost nullable-reference annotation: an
/// `Annotated` reference gets the C#-style postfix `?` (`string?`,
/// `(int list)?`); `NotAnnotated`/`Oblivious` render exactly as [`format_type`].
pub fn format_nullable_type(nt: &NullableType, scope: &TyparScope) -> String {
    render_nullable(nt, scope).0
}

/// Render a member as a one-line F# signature, e.g.
/// `static member WriteLine: value: string -> unit`, `member Count: int with
/// get`, `val mutable x: int`, `new: value: string -> Container`. `owner`
/// supplies the type-parameter scope (so `'T` resolves) and selects the keyword
/// family: a [`EntityKind::Module`] owner renders `val …` (F# functions/values);
/// any other renders `member` / `static member` / `abstract member` / `new`.
///
/// This is the F# *signature* view. Optional parameters are distinguished by
/// dialect — F# `?name: T` vs a .NET `[<Optional>] name: T` (C# default), whose
/// value renders inline (`name: T = 5`) — indexer index-parameters render as the
/// index dimension (`member Item: i: int -> 'T`), and nullable-reference
/// annotations render the C#-style `?` (`string?`), and a `params T[]`
/// parameter carries the `[<ParamArray>]` attribute prefix. Detail with no
/// faithful F# signature syntax is still not surfaced: the member-level
/// `required` (C# 11) and `extension` flags, which the hover handler puts on the
/// context line instead. See `docs/hover-signature-plan.md` for the follow-ups.
pub fn format_member(member: &Member, owner: &Entity) -> String {
    match member {
        Member::Method(m) => format_method(m, owner),
        Member::Field(f) => format_field(f, owner),
        Member::Property(p) => format_property(p, owner),
        Member::Event(e) => format_event(e, owner),
    }
}

/// Render an entity's F# declaration head: `type List<'T>`, `module Operators`,
/// `exception MyError`, `[<Measure>] type kg`, with F# attribute prefixes for
/// the model's marker flags (`[<Struct>]`, `[<IsReadOnly>]`, `[<IsByRefLike>]`,
/// `[<AutoOpen>]`, `[<RequireQualifiedAccess>]`). The short (source) name is
/// used; the namespace is the caller's context.
///
/// The keyword is `type` for the whole class/struct/union/record/enum/delegate/
/// abbreviation family (F# has no per-kind keyword), so a consumer wanting to
/// distinguish e.g. a record from a union must surface that separately.
pub fn format_entity_header(entity: &Entity) -> String {
    let mut attrs: Vec<&str> = Vec::new();
    if entity.kind == EntityKind::Measure {
        attrs.push("Measure");
    }
    // `[<Struct>]` conveys the value-type-ness the `type` keyword can't — for a
    // plain struct and for `[<Struct>]` records/unions. An enum is already a
    // value type and `[<Struct>]` is not valid on it.
    if entity.is_struct && entity.kind != EntityKind::Enum {
        attrs.push("Struct");
    }
    if entity.is_readonly {
        attrs.push("IsReadOnly");
    }
    if entity.is_byref_like {
        attrs.push("IsByRefLike");
    }
    if entity.is_auto_open {
        attrs.push("AutoOpen");
    }
    if entity.is_require_qualified_access {
        attrs.push("RequireQualifiedAccess");
    }
    let prefix = if attrs.is_empty() {
        String::new()
    } else {
        format!("[<{}>] ", attrs.join("; "))
    };

    let keyword = match entity.kind {
        EntityKind::Module => "module",
        EntityKind::Exception => "exception",
        _ => "type",
    };
    let name = entity.source_name.as_deref().unwrap_or(&entity.name);
    format!(
        "{prefix}{keyword} {name}{}",
        format_typar_list(&entity.generic_parameters)
    )
}

fn format_method(m: &MethodLike, owner: &Entity) -> String {
    let scope = TyparScope::new(&owner.generic_parameters, &m.generic_parameters);
    let params = format_params(&m.signature.parameters, &scope);

    // A constructor returns its declaring type, not the `void` the IL records.
    if m.is_constructor {
        return format!("new: {params} -> {}", owner_name_with_typars(owner));
    }

    let keyword = method_keyword(m, owner);
    let name = m.source_name.as_deref().unwrap_or(&m.name);
    let typars = format_typar_list(&m.generic_parameters);
    let ret = format_return(
        &m.signature.return_type,
        m.signature.return_nullability,
        &scope,
    );

    // An F# module-level `let` *value* — a property the projector rebranded as a
    // 0-parameter method — renders as a value (`val [mutable] x: T`), not a
    // function. `is_mutable` recovers the `let mutable` setter the rebrand drops.
    // This covers *non-generic* values, which F# emits as property getters.
    if let Some(mv) = m.module_value {
        let mutable = if mv.is_mutable { "mutable " } else { "" };
        return format!("{keyword} {mutable}{name}{typars}: {ret}");
    }
    // A *generic* module value (`let empty<'T> = …`) is emitted as a
    // 0-parameter generic *method*, not a property, so the rebrand above never
    // tags it — yet it is still a value (`val empty<'T>: 'T[]`), not a unit
    // function. Treat a 0-parameter generic module method as a value: a generic
    // 0-parameter *unit-function* (`let f<'T> () = …`) is vanishingly rare, and
    // distinguishing the two needs the pickle's `ValReprInfo` arity (follow-up).
    if owner.kind == EntityKind::Module
        && m.signature.parameters.is_empty()
        && !m.generic_parameters.is_empty()
    {
        return format!("{keyword} {name}{typars}: {ret}");
    }
    // Otherwise a genuine function/method. A 0-parameter module function
    // (`let f () = …`) is `val f: unit -> T` — `format_params` yields `unit` for
    // the empty list — no longer mis-collapsed to a value by the old heuristic.
    format!("{keyword} {name}{typars}: {params} -> {ret}")
}

/// `val` for a module-level binding, otherwise the `member` family with its
/// modifiers — `static` and `abstract` combine (a static-abstract interface
/// member, IWSAM, is `static abstract member`, not just `static member`).
fn method_keyword(m: &MethodLike, owner: &Entity) -> String {
    if owner.kind == EntityKind::Module {
        return "val".to_string();
    }
    let mut parts: Vec<&str> = Vec::new();
    if m.is_static {
        parts.push("static");
    }
    if m.is_abstract {
        parts.push("abstract");
    }
    parts.push("member");
    parts.join(" ")
}

/// The parameters as an F# domain: `a: int * b: string`, or `unit` when empty.
/// A nameless parameter renders as its bare type. (IL records tupled
/// parameters; the source-level currying of an F# function isn't recoverable
/// from metadata, so this is the honest tupled view.)
fn format_params(params: &[Parameter], scope: &TyparScope) -> String {
    if params.is_empty() {
        return "unit".to_string();
    }
    params
        .iter()
        .map(|p| format_param(p, scope))
        .collect::<Vec<_>>()
        .join(" * ")
}

/// One parameter with its optional/default form folded in: F# `?name: T` for an
/// `[<OptionalArgument>]` parameter (unwrapping the `FSharpOption<T>` it is typed
/// as), `name: T = <value>` for a C# default, `[<Optional>] name: T` for a
/// value-less .NET / COM optional, else `name: T`. A `params T[]` parameter
/// additionally gets a leading `[<ParamArray>]` — an *orthogonal* marker that
/// composes with any of the above (`[<ParamArray>] ?name: T[]`). A nameless
/// parameter drops the `name:`.
fn format_param(p: &Parameter, scope: &TyparScope) -> String {
    // `[<ParamArray>]` (C#'s `params`) is *orthogonal* to the optional/default
    // forms, not exclusive with them: F# lets a params array also be `[<Optional>]`
    // or an F# `?optional`, and the compiler then emits `ParamArrayAttribute`
    // *alongside* the optional flag (checked with `dotnet fsi`; FCS itself renders
    // `[<ParamArray>] ?xs: 'T[]`). So the projector can set `is_param_array` under
    // any `ParamDefault`, and the marker is prepended to whatever the default arm
    // renders — never gated on a single arm, which would silently drop it.
    let param_array = if p.is_param_array {
        "[<ParamArray>] "
    } else {
        ""
    };
    let rendered = match &p.default {
        ParamDefault::FSharpOptional => {
            // Unwrap the `FSharpOption<T>` to its argument, keeping that
            // argument's own nullability (`?name: string?`). A malformed
            // `[<OptionalArgument>]` on a non-option type falls back to the
            // declared type with the parameter's nullability.
            let fallback = NullableType {
                ty: p.ty.clone(),
                nullability: p.nullability,
            };
            let inner = fsharp_option_inner(&p.ty).unwrap_or(&fallback);
            prefix_named(
                "?",
                &wrap_nullable(Prec::Postfix, inner, scope),
                p.name.as_deref(),
            )
        }
        // A C# default value renders inline (`name: T = 5`); the `= value`
        // already marks it a .NET default, so the `[<Optional>]` prefix is
        // dropped.
        ParamDefault::Optional(Some(value)) => {
            let ty = format_param_type(p, scope);
            let value = render_constant(value);
            match &p.name {
                Some(name) => format!("{name}: {ty} = {value}"),
                None => format!("{ty} = {value}"),
            }
        }
        // A value-less `[Optional]` / COM optional keeps the attribute marker.
        ParamDefault::Optional(None) => prefix_named(
            "[<Optional>] ",
            &format_param_type(p, scope),
            p.name.as_deref(),
        ),
        ParamDefault::None => prefix_named("", &format_param_type(p, scope), p.name.as_deref()),
    };
    format!("{param_array}{rendered}")
}

/// Render a [`ConstantValue`] as an F#-ish literal for a default value.
fn render_constant(value: &ConstantValue) -> String {
    match value {
        ConstantValue::Bool(b) => b.to_string(),
        // `Debug` quotes + escapes: `'c'` for chars, `"s"` for strings.
        ConstantValue::Char(c) => format!("{c:?}"),
        ConstantValue::Int(i) => i.to_string(),
        ConstantValue::UInt(u) => u.to_string(),
        ConstantValue::F32(bits) => f32::from_bits(*bits).to_string(),
        ConstantValue::F64(bits) => f64::from_bits(*bits).to_string(),
        ConstantValue::String(s) => format!("{s:?}"),
        ConstantValue::Null => "null".to_string(),
        ConstantValue::Decimal {
            negative,
            scale,
            mantissa,
        } => render_decimal(*negative, *scale, *mantissa),
        // No F# `DateTime` literal exists; render the ticks as the constructor
        // call F# would accept (`L` = the `int64` the ctor takes).
        ConstantValue::DateTime(ticks) => format!("System.DateTime({ticks}L)"),
    }
}

/// Render a `System.Decimal` as its F# literal (`1.5M`): the integer `mantissa`
/// with a decimal point `scale` digits from the right, an optional sign, and the
/// `M` suffix. The declared `scale` is honoured, so `1.50m` (scale 2) renders
/// `1.50M`, distinct from `1.5m`. No floating point — the value is exact.
fn render_decimal(negative: bool, scale: u8, mantissa: u128) -> String {
    let digits = mantissa.to_string();
    let scale = scale as usize;
    let magnitude = if scale == 0 {
        digits
    } else if digits.len() > scale {
        let point = digits.len() - scale;
        format!("{}.{}", &digits[..point], &digits[point..])
    } else {
        // Fewer integer digits than the scale: pad with leading zeros after
        // `0.` (`mantissa = 5, scale = 3` ⇒ `0.005`).
        format!("0.{}{}", "0".repeat(scale - digits.len()), digits)
    };
    // A zero value has no sign (`-0` would be misleading); decimal's negative
    // zero is rendered as `0M`.
    let sign = if negative && mantissa != 0 { "-" } else { "" };
    format!("{sign}{magnitude}M")
}

/// `{prefix}{name}: {ty}`, or `{prefix}{ty}` for a nameless parameter.
fn prefix_named(prefix: &str, ty: &str, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{prefix}{name}: {ty}"),
        None => format!("{prefix}{ty}"),
    }
}

/// The `T` of an `FSharpOption<T>` — how F# types a `?x: T` optional parameter —
/// or `None` when `ty` is not a single-argument `FSharpOption`.
fn fsharp_option_inner(ty: &TypeRef) -> Option<&NullableType> {
    match ty {
        TypeRef::Named {
            namespace,
            name,
            type_args,
            ..
        } if name == "FSharpOption"
            && type_args.len() == 1
            && namespace.join(".") == "Microsoft.FSharp.Core" =>
        {
            Some(&type_args[0])
        }
        _ => None,
    }
}

/// A parameter's type as it appears in a signature's domain. An `out` parameter
/// (byref + `[Out]`) reads `outref<T>`, a read-only one (byref +
/// `modreq(InAttribute)` — C#'s `in` / `ref readonly`) `inref<T>`, and a plain
/// byref `byref<T>` — the projector strips the byref into
/// [`Parameter::is_byref`]/`is_out`/`is_readonly_ref`, leaving the referent in
/// `ty`, so it is folded back here. Otherwise the type renders at postfix
/// precedence so a function-typed parameter is parenthesised (`(int -> string)`)
/// rather than collapsing into the method's own `->`.
fn format_param_type(p: &Parameter, scope: &TyparScope) -> String {
    // The parameter's `[Nullable]` annotation describes the type (the referent,
    // for a byref/out), so it rides inside the `outref<>`/`byref<>` wrapper.
    let nt = NullableType {
        ty: p.ty.clone(),
        nullability: p.nullability,
    };
    if p.is_out {
        format!("outref<{}>", format_nullable_type(&nt, scope))
    } else if p.is_byref {
        format!(
            "{}<{}>",
            byref_tycon(p.is_readonly_ref),
            format_nullable_type(&nt, scope)
        )
    } else {
        wrap_nullable(Prec::Postfix, &nt, scope)
    }
}

/// A return type, mapping the IL `void` to F#'s `unit`, and surfacing the
/// return position's nullable annotation (`string?`). A byref return keeps its
/// `ByRef` (unlike a byref *parameter*, which the projector strips); the
/// annotation lands on the referent (`byref<string?>`), which
/// [`render_nullable`] handles.
fn format_return(ret: &TypeRef, nullability: Nullability, scope: &TyparScope) -> String {
    match ret {
        TypeRef::Primitive(Primitive::Void) => "unit".to_string(),
        other => format_nullable_type(
            &NullableType {
                ty: other.clone(),
                nullability,
            },
            scope,
        ),
    }
}

/// `<'a, 'b>` for a non-empty typar list, else empty.
fn format_typar_list(typars: &[TypeParameter]) -> String {
    if typars.is_empty() {
        return String::new();
    }
    let names = typars
        .iter()
        .map(|t| format!("'{}", t.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!("<{names}>")
}

/// The owner's short F# name with its type parameters: `List<'T>`. Used as a
/// constructor's return type.
fn owner_name_with_typars(owner: &Entity) -> String {
    let name = owner.source_name.as_deref().unwrap_or(&owner.name);
    format!("{name}{}", format_typar_list(&owner.generic_parameters))
}

fn format_field(f: &Field, owner: &Entity) -> String {
    let scope = TyparScope::new(&owner.generic_parameters, &[]);
    // A `[<Literal>]` const (C# `const`, F# `[<Literal>]`, an enum case) is a
    // compile-time constant — marked, and never `mutable` even though
    // `is_init_only` is false (the CLR uses the `Literal` flag, not `initonly`).
    let literal = if f.is_literal { "[<Literal>] " } else { "" };
    // A `volatile` field (`modreq(IsVolatile)` on its type) is F#'s
    // `[<VolatileField>]`. Without it the hover would read identically to an
    // ordinary mutable field, hiding the memory-model difference the projector
    // went to the trouble of not dropping. Mutually exclusive with `[<Literal>]`
    // (a constant has no storage to be volatile about).
    let volatile = if f.is_volatile {
        "[<VolatileField>] "
    } else {
        ""
    };
    let static_ = if f.is_static { "static " } else { "" };
    // Mutable unless read-only (`initonly`) or a literal constant — so a genuine
    // `static` mutable field now reads `mutable`, which the old `!is_static`
    // heuristic could not express.
    let mutable = if !f.is_init_only && !f.is_literal {
        "mutable "
    } else {
        ""
    };
    format!(
        "{literal}{volatile}{static_}val {mutable}{}: {}",
        f.name,
        format_nullable_type(
            &NullableType {
                ty: f.ty.clone(),
                nullability: f.nullability,
            },
            &scope
        )
    )
}

fn format_property(p: &Property, owner: &Entity) -> String {
    let scope = TyparScope::new(&owner.generic_parameters, &[]);
    let ty = format_nullable_type(
        &NullableType {
            ty: p.ty.clone(),
            nullability: p.nullability,
        },
        &scope,
    );

    // An F# module-level `let`/`let mutable` value compiles to a property; read
    // it back as the `val` it was written as, not `member … with get`.
    if owner.kind == EntityKind::Module {
        let mutable = if p.has_setter { "mutable " } else { "" };
        return format!("val {mutable}{}: {ty}", p.name);
    }

    let keyword = if p.is_static {
        "static member"
    } else {
        "member"
    };
    let accessors = match (p.has_getter, p.has_setter) {
        (true, true) => " with get, set",
        (true, false) => " with get",
        (false, true) => " with set",
        (false, false) => "",
    };
    // An indexer (`this[i]`) carries its index dimension before the element
    // type, exactly like a method's parameters: `member Item: i: int -> 'T`.
    // Multiple indices tuple with `*` (`x: int * y: int -> 'T`). An ordinary
    // property has no index dimension and renders `member Name: T`.
    if p.parameters.is_empty() {
        format!("{keyword} {}: {ty}{accessors}", p.name)
    } else {
        let indices = format_index_params(&p.parameters, &scope);
        format!("{keyword} {}: {indices} -> {ty}{accessors}", p.name)
    }
}

/// The index dimension of an indexer: each parameter as `name: T` (bare `T`
/// when the accessor carried no name), `*`-tupled like a method's parameter
/// list. Never empty — the caller guards on `parameters.is_empty()`.
fn format_index_params(params: &[IndexParameter], scope: &TyparScope) -> String {
    params
        .iter()
        .map(|p| {
            // A `params`/`[<ParamArray>]` indexer (`this[params int[] xs]`) carries
            // the marker on its index parameter, exactly like a method parameter.
            let param_array = if p.is_param_array {
                "[<ParamArray>] "
            } else {
                ""
            };
            prefix_named(
                param_array,
                &wrap_nullable(Prec::Postfix, &p.ty, scope),
                p.name.as_deref(),
            )
        })
        .collect::<Vec<_>>()
        .join(" * ")
}

fn format_event(e: &Event, owner: &Entity) -> String {
    let scope = TyparScope::new(&owner.generic_parameters, &[]);
    let keyword = if e.is_static {
        "static member"
    } else {
        "member"
    };
    // `[<CLIEvent>]` marks the member as an event rather than a plain member
    // returning a delegate — the F#-idiomatic way to declare a CLI event, and
    // the only way to keep the event-ness the signature would otherwise hide.
    format!(
        "[<CLIEvent>] {keyword} {}: {}",
        e.name,
        format_nullable_type(
            &NullableType {
                ty: e.delegate_type.clone(),
                nullability: e.nullability,
            },
            &scope
        )
    )
}

/// Precedence of a rendered type, used to decide parenthesisation. Lower
/// binds looser: a function (`a -> b`) must be parenthesised wherever a
/// tighter context is expected.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    /// `a -> b` — the loosest.
    Arrow,
    /// `a name` — a postfix generic application (`int list`).
    Postfix,
    /// A self-delimiting form: primitive, typar, `Name<..>`, array, `byref<>`,
    /// or an always-parenthesised tuple.
    Atom,
}

/// Render `ty`, parenthesising it if its precedence is looser than `min`.
fn wrap(min: Prec, ty: &TypeRef, scope: &TyparScope) -> String {
    let (s, prec) = render(ty, scope);
    if prec < min { format!("({s})") } else { s }
}

/// Render a type with its nullable-reference annotation. An `Annotated`
/// reference renders the C#-style postfix `?` (the operand parenthesised at
/// [`Prec::Atom`] so `(int list)?` reads correctly, while an atom stays bare —
/// `string?`); the result is itself an [`Prec::Atom`], so `string?[]` and
/// `List<string?>` need no further parens. `NotAnnotated`/`Oblivious` render
/// exactly as the bare [`render`], so output is unchanged wherever a type is not
/// annotated. (Value types are always oblivious, so they are never marked.)
///
/// A byref (`ref`) carries no annotation of its own — the model records the
/// *referent's* nullability on the byref position — so an annotated byref puts
/// the `?` inside the wrapper (`byref<string?>`, never `byref<string>?`). This
/// keeps the public [`format_nullable_type`] correct for a byref return, not
/// just the member-formatter path.
fn render_nullable(nt: &NullableType, scope: &TyparScope) -> (String, Prec) {
    match nt.nullability {
        Nullability::Annotated => match &nt.ty {
            TypeRef::ByRef { inner, readonly } => {
                let inner = NullableType {
                    ty: (**inner).clone(),
                    nullability: Nullability::Annotated,
                };
                (
                    format!(
                        "{}<{}>",
                        byref_tycon(*readonly),
                        render_nullable(&inner, scope).0
                    ),
                    Prec::Atom,
                )
            }
            _ => (format!("{}?", wrap(Prec::Atom, &nt.ty, scope)), Prec::Atom),
        },
        Nullability::NotAnnotated | Nullability::Oblivious => render(&nt.ty, scope),
    }
}

/// Like [`wrap`], but threading the position's nullability: render the type with
/// its `?` (if annotated), then parenthesise if looser than `min`.
fn wrap_nullable(min: Prec, nt: &NullableType, scope: &TyparScope) -> String {
    let (s, prec) = render_nullable(nt, scope);
    if prec < min { format!("({s})") } else { s }
}

/// The F# type constructor for a byref: a read-only one (`modreq(InAttribute)`
/// over the byref — C#'s `in` / `ref readonly`) is F#'s `inref<'T>`; a writable
/// one is `byref<'T>`. F# also has `outref<'T>`, but out-ness is a *parameter*
/// flag rather than part of the type, so it is the member formatter's to render,
/// not this function's.
fn byref_tycon(readonly: bool) -> &'static str {
    if readonly { "inref" } else { "byref" }
}

fn render(ty: &TypeRef, scope: &TyparScope) -> (String, Prec) {
    match ty {
        TypeRef::Primitive(p) => (primitive_name(*p).to_string(), Prec::Atom),
        TypeRef::Var { index, is_method } => (scope.var_name(*index, *is_method), Prec::Atom),
        // `sizes` / `lower_bounds` are dropped on purpose (see the module-level
        // "Deliberate approximations").
        TypeRef::Array { element, rank, .. } => {
            // `T[]`, `T[,]`, … — the element must read as an atom so a postfix
            // generic or function element is parenthesised (`(int list)[]`); a
            // nullable element keeps its `?` (`string?[]`).
            let elem = wrap_nullable(Prec::Atom, element, scope);
            let commas = ",".repeat((*rank as usize).saturating_sub(1));
            (format!("{elem}[{commas}]"), Prec::Atom)
        }
        // The `<..>` is self-delimiting, so the inner type renders plainly.
        TypeRef::ByRef { inner, readonly } => (
            format!("{}<{}>", byref_tycon(*readonly), render(inner, scope).0),
            Prec::Atom,
        ),
        TypeRef::Ptr(Some(inner)) => (format!("nativeptr<{}>", render(inner, scope).0), Prec::Atom),
        TypeRef::Ptr(None) => ("voidptr".to_string(), Prec::Atom),
        TypeRef::Named {
            namespace,
            name,
            type_args,
            segment_arities,
            ..
        } => render_named(namespace, name, type_args, segment_arities, scope),
    }
}

fn render_named(
    namespace: &[String],
    name: &str,
    type_args: &[NullableType],
    segment_arities: &[usize],
    scope: &TyparScope,
) -> (String, Prec) {
    let ns = namespace.join(".");

    // `FSharpFunc<A, B>` → `A -> B`. Right-associative: the codomain renders
    // plainly so curried functions chain (`a -> b -> c`); the domain renders
    // at postfix precedence so a function argument is parenthesised.
    if ns == "Microsoft.FSharp.Core" && name == "FSharpFunc" && type_args.len() == 2 {
        let from = wrap_nullable(Prec::Postfix, &type_args[0], scope);
        let to = render_nullable(&type_args[1], scope).0;
        return (format!("{from} -> {to}"), Prec::Arrow);
    }

    // `System.Tuple<..>` / `System.ValueTuple<..>` → `(a * b)` / `struct (a * b)`,
    // flattening the CLR `TRest` chain first. Always parenthesised, so the
    // result is an atom.
    if let Some(is_struct) = tuple_kind(&ns, name)
        && type_args.len() >= 2
    {
        let body = flatten_tuple(type_args)
            .iter()
            .map(|a| wrap_nullable(Prec::Postfix, a, scope))
            .collect::<Vec<_>>()
            .join(" * ");
        let s = if is_struct {
            format!("struct ({body})")
        } else {
            format!("({body})")
        };
        return (s, Prec::Atom);
    }

    // A nested type's IL name is the enclosing chain joined with `/`
    // (`Outer/Inner`, per `qualified_typedef_name`); F# accesses a nested type
    // with `.`, placing each generic argument on its declaring segment.
    if name.contains('/')
        && let Some(rendered) = render_nested(name, type_args, segment_arities, scope)
    {
        return (rendered, Prec::Atom);
    }

    // Single segment — or a nested name whose arities are inconsistent with
    // `type_args` (corrupt metadata; `render_nested` returned `None`), in which
    // case we fall back to the naive arrangement on the whole dotted name.
    // Abbreviations are all top-level, so a `/` only ever reaches this branch
    // via that fallback.
    let display = match abbreviation(&ns, name) {
        Some(abbr) => abbr.to_string(),
        None => name.replace('/', "."),
    };
    match type_args.len() {
        0 => (display, Prec::Atom),
        // One argument: postfix (`int list`). The argument renders at postfix
        // precedence so a function argument is parenthesised but a nested
        // postfix generic chains (`int list option`).
        1 => {
            let arg = wrap_nullable(Prec::Postfix, &type_args[0], scope);
            (format!("{arg} {display}"), Prec::Postfix)
        }
        // Two or more: angle brackets, which delimit the arguments.
        _ => {
            let args = type_args
                .iter()
                .map(|a| render_nullable(a, scope).0)
                .collect::<Vec<_>>()
                .join(", ");
            (format!("{display}<{args}>"), Prec::Atom)
        }
    }
}

/// `Some(true)` for `System.ValueTuple`, `Some(false)` for `System.Tuple`,
/// `None` for any other type.
fn tuple_kind(ns: &str, name: &str) -> Option<bool> {
    match (ns, name) {
        ("System", "ValueTuple") => Some(true),
        ("System", "Tuple") => Some(false),
        _ => None,
    }
}

/// Recover the logical element list of a CLR tuple, flattening its `TRest`
/// chain. An arity-8 tuple stores its overflow elements in a nested tuple in
/// the final generic slot (`(Value)Tuple<T1..T7, TRest>`), so an 8+ element F#
/// tuple round-trips through eight-wide CLR groups. Shorter tuples — and a
/// genuine nested tuple anywhere but that arity-8 `TRest` slot — are returned
/// unchanged (the CLR reserves the 8th slot for `TRest`, so the flatten is
/// unambiguous).
fn flatten_tuple(args: &[NullableType]) -> Vec<&NullableType> {
    if args.len() == 8
        && let TypeRef::Named {
            namespace,
            name,
            type_args,
            ..
        } = &args[7].ty
        && tuple_kind(&namespace.join("."), name).is_some()
    {
        let mut out: Vec<&NullableType> = args[..7].iter().collect();
        out.extend(flatten_tuple(type_args));
        return out;
    }
    args.iter().collect()
}

/// Render a nested type (`name` contains `/`), placing each generic argument on
/// the segment that declares it via the per-segment `segment_arities`:
/// `Dictionary`2/Enumerator` + `[2, 0]` + `[int, string]` → `Dictionary<int,
/// string>.Enumerator`; `Outer`1/Inner`1` + `[1, 1]` → `Outer<…>.Inner<…>`. Each
/// segment with arity ≥ 1 is angle-bracketed (postfix would read badly inside a
/// dotted chain); segments join with `.`.
///
/// Returns `None` — so the caller falls back to a naive whole-name rendering —
/// when the model is inconsistent (one arity per segment is required, and the
/// arities must sum to `type_args.len()`). That only happens for corrupt
/// metadata; the projector records arities faithfully without enforcing the
/// relationship, so the renderer degrades rather than panicking.
fn render_nested(
    name: &str,
    type_args: &[NullableType],
    segment_arities: &[usize],
    scope: &TyparScope,
) -> Option<String> {
    let segments: Vec<&str> = name.split('/').collect();
    // Saturating, not `sum()`: adversarial metadata can carry arity suffixes
    // whose sum overflows `usize` (an unchecked `sum()` panics in debug, wraps
    // in release). A saturated sum can never equal a real `type_args.len()`, so
    // the mismatch check below routes such input to the fallback as documented.
    let arity_sum = segment_arities
        .iter()
        .fold(0usize, |acc, &a| acc.saturating_add(a));
    if segment_arities.len() != segments.len() || arity_sum != type_args.len() {
        return None;
    }
    let mut args = type_args.iter();
    let mut out = Vec::with_capacity(segments.len());
    for (seg, &arity) in segments.iter().zip(segment_arities) {
        if arity == 0 {
            out.push((*seg).to_string());
        } else {
            let mut seg_args = Vec::with_capacity(arity);
            for _ in 0..arity {
                // The `<..>` delimits the arguments, so each renders plainly.
                seg_args.push(render_nullable(args.next()?, scope).0);
            }
            out.push(format!("{seg}<{}>", seg_args.join(", ")));
        }
    }
    Some(out.join("."))
}

/// The F# keyword/alias for a well-known **BCL** type, keyed on its dotted
/// namespace and simple name (`("System", "Int32") -> "int"`). The single source
/// of truth for the .NET-primitive ↔ F#-alias mapping in this crate: both
/// `primitive_name` (the `TypeRef::Primitive` path, via `primitive_fqn`) and the
/// `abbreviation` System arms route through it, and `borzoi-sema`'s
/// `Ty::render_fsharp` consults it so a hovered literal and a hovered member
/// render the same alias (`uint`, not `uint32`). `None` for anything without a
/// distinguished F# spelling. Excludes the F#-specific abbreviations
/// (`list`/`option`/`seq`/…), which are not BCL types and live in
/// `abbreviation`.
pub fn fsharp_alias(namespace: &str, name: &str) -> Option<&'static str> {
    Some(match (namespace, name) {
        ("System", "Void") => "unit",
        ("System", "Boolean") => "bool",
        ("System", "Char") => "char",
        ("System", "SByte") => "sbyte",
        ("System", "Byte") => "byte",
        ("System", "Int16") => "int16",
        ("System", "UInt16") => "uint16",
        ("System", "Int32") => "int",
        ("System", "UInt32") => "uint",
        ("System", "Int64") => "int64",
        ("System", "UInt64") => "uint64",
        ("System", "Single") => "float32",
        ("System", "Double") => "float",
        ("System", "IntPtr") => "nativeint",
        ("System", "UIntPtr") => "unativeint",
        ("System", "Object") => "obj",
        ("System", "String") => "string",
        ("System", "Decimal") => "decimal",
        _ => return None,
    })
}

/// The BCL `(namespace, name)` an ECMA-335 [`Primitive`] denotes — the bridge
/// from the IL element-type code to [`fsharp_alias`]'s FQN key, so the alias
/// table is stated once.
fn primitive_fqn(p: Primitive) -> (&'static str, &'static str) {
    match p {
        Primitive::Void => ("System", "Void"),
        Primitive::Bool => ("System", "Boolean"),
        Primitive::Char => ("System", "Char"),
        Primitive::I1 => ("System", "SByte"),
        Primitive::U1 => ("System", "Byte"),
        Primitive::I2 => ("System", "Int16"),
        Primitive::U2 => ("System", "UInt16"),
        Primitive::I4 => ("System", "Int32"),
        Primitive::U4 => ("System", "UInt32"),
        Primitive::I8 => ("System", "Int64"),
        Primitive::U8 => ("System", "UInt64"),
        Primitive::R4 => ("System", "Single"),
        Primitive::R8 => ("System", "Double"),
        Primitive::IntPtr => ("System", "IntPtr"),
        Primitive::UIntPtr => ("System", "UIntPtr"),
        Primitive::Object => ("System", "Object"),
        Primitive::String => ("System", "String"),
    }
}

/// F# alias for an ECMA-335 primitive. Every primitive has one (see
/// [`fsharp_alias`] / [`primitive_fqn`]), so the lookup is total.
fn primitive_name(p: Primitive) -> &'static str {
    let (ns, name) = primitive_fqn(p);
    fsharp_alias(ns, name).expect("every primitive has an F# alias")
}

/// The F# source name for a well-known nominal type, keyed on its
/// dotted namespace and IL name (arity backtick already stripped by the
/// model). `None` falls back to the short IL name. The BCL System arms defer to
/// [`fsharp_alias`]; the entries here are the F#-specific abbreviations and the
/// `System.Decimal` value type (which has no `ELEMENT_TYPE_*` code, so it
/// arrives as a named type rather than via [`primitive_name`]).
fn abbreviation(ns: &str, name: &str) -> Option<&'static str> {
    Some(match (ns, name) {
        ("Microsoft.FSharp.Collections", "FSharpList") => "list",
        ("Microsoft.FSharp.Core", "FSharpOption") => "option",
        ("Microsoft.FSharp.Core", "FSharpRef") => "ref",
        ("Microsoft.FSharp.Collections", "FSharpMap") => "Map",
        ("System.Collections.Generic", "IEnumerable") => "seq",
        ("System.Numerics", "BigInteger") => "bigint",
        ("Microsoft.FSharp.Core", "Unit") => "unit",
        ("System", "Object") | ("System", "String") | ("System", "Decimal") => {
            return fsharp_alias(ns, name);
        }
        _ => return None,
    })
}
