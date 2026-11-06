//! Whether a type's metadata is shaped as the C# compiler writes it.
//!
//! A consumer that reproduces another metadata reader exactly (the LSP's
//! `<inheritdoc>` expansion reproduces Roslyn's PE importer) can only promise
//! that on metadata whose shape it knows. ECMA-335 admits far more than C#
//! writes — an event's `fire` accessor (VB's custom events, C++/CLI),
//! accessors named apart from their property, an optional modifier
//! (C++/CLI), a method type parameter its method does not declare, names
//! post-compile tools write (`!XamlIlPopulate`) — and on those the two
//! readers' agreement is incidental. [`compiler_shape`] is the boundary: it
//! accepts a type only when every part of it matches a shape C# writes, so a
//! shape nobody has thought of is unshaped by default rather than modelled
//! by accident. VB and F# write a subset besides their own shapes; F#'s
//! assemblies are read in F#'s source view, which this does not judge.
//!
//! It judges a type's surface to other assemblies: its name, its base types,
//! and every member another assembly can bind — one visible outside the
//! assembly, a virtual one, or one implementing an interface member through
//! a `MethodImpl` row. A private non-virtual member (a lambda's
//! `<.cctor>b__0_0`, a backing field) is no other assembly's concern. It
//! reads only the projected [`Entity`], and judges the type alone (its
//! nested types are entities of their own). What makes metadata hard to read
//! rather than unusual — a dropped member, a No-PIA local type — is the
//! consumer's to weigh: compilers write both.

use crate::model::{
    Access, AccessorSlot, Entity, Event, ImplementedMember, InterfaceMemberImpl, Member,
    MethodLike, Property, TypeRef,
};

/// The first part of a type that no compiler writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unshaped {
    /// The type's name or a namespace segment is empty or holds a character
    /// a documentation ID or a type name gives another meaning (`.`, `,`,
    /// `#`, `` ` ``, brackets, whitespace), or keeps a non-canonical arity
    /// suffix.
    TypeName,
    /// A member's name is empty or holds such a character, outside an
    /// explicit implementation's qualified name.
    MemberName,
    /// A signature names a type parameter (`!i`, `!!i`) its owner does not
    /// declare.
    TypeParameterIndex,
    /// A signature carries an optional custom modifier: C# and VB write
    /// none outside function pointers, F# none at all.
    OptionalModifier,
    /// A property's own signature disagrees with its accessors'.
    PropertySignature,
    /// An event's adder or remover does not take exactly the event's
    /// delegate type and return nothing.
    EventSignature,
    /// An event with a `fire` accessor (VB's custom events and C++/CLI write
    /// one).
    FireAccessor,
    /// A property's or event's accessors are not one method per role,
    /// named by the `get_`/`set_`/`add_`/`remove_` convention after the
    /// member, or a `MethodImpl` row on an accessor names another role's
    /// accessor.
    Accessors,
}

/// `entity`'s verdict: `Ok` when every part of it is compiler-shaped, else
/// the first part that is not.
pub fn compiler_shape(entity: &Entity) -> Result<(), Unshaped> {
    let type_arity = entity.generic_parameters.len();
    if !entity.namespace.iter().all(|s| plain_name(s)) || !plain_name(&entity.name) {
        return Err(Unshaped::TypeName);
    }
    let in_range = |ty: &TypeRef, method_arity: usize| vars_in_range(ty, type_arity, method_arity);
    if !entity
        .base_type
        .iter()
        .chain(&entity.interfaces)
        .all(|t| in_range(t, 0))
    {
        return Err(Unshaped::TypeParameterIndex);
    }
    for member in entity.members.iter().filter(|m| bindable(m)) {
        match member {
            Member::Method(m) => method_shape(m, type_arity)?,
            Member::Field(f) => {
                if !member_name(&f.name, false) {
                    return Err(Unshaped::MemberName);
                }
                if !in_range(&f.ty, 0) {
                    return Err(Unshaped::TypeParameterIndex);
                }
            }
            Member::Property(p) => property_shape(p, type_arity)?,
            Member::Event(e) => event_shape(e, type_arity)?,
        }
    }
    Ok(())
}

/// Whether another assembly can bind `member`: it is visible outside its
/// assembly, virtual, or implements an interface member through a
/// `MethodImpl` row.
fn bindable(member: &Member) -> bool {
    let visible = |access: Access| !matches!(access, Access::Private | Access::Internal);
    let slots_virtual = |slots: &[AccessorSlot]| slots.iter().any(|s| s.is_virtual);
    match member {
        Member::Method(m) => {
            visible(m.access)
                || m.is_virtual
                || !m.implements.is_empty()
                || !m.unclassified_impls.is_empty()
        }
        Member::Field(f) => visible(f.access),
        Member::Property(p) => {
            visible(p.access)
                || slots_virtual(&p.accessor_slots)
                || !p.implements.is_empty()
                || !p.unclassified_impls.is_empty()
        }
        Member::Event(e) => {
            visible(e.access)
                || slots_virtual(&e.accessor_slots)
                || !e.implements.is_empty()
                || !e.unclassified_impls.is_empty()
        }
    }
}

fn method_shape(m: &MethodLike, type_arity: usize) -> Result<(), Unshaped> {
    let explicit = !m.implements.is_empty() || !m.unclassified_impls.is_empty();
    if !(m.is_constructor && matches!(m.name.as_str(), ".ctor" | ".cctor"))
        && !member_name(&m.name, explicit)
    {
        return Err(Unshaped::MemberName);
    }
    let arity = m.generic_parameters.len();
    if !std::iter::once(&m.signature.return_type)
        .chain(m.signature.parameters.iter().map(|p| &p.ty))
        .all(|t| vars_in_range(t, type_arity, arity))
    {
        return Err(Unshaped::TypeParameterIndex);
    }
    if m.drops_optional_modifier {
        return Err(Unshaped::OptionalModifier);
    }
    Ok(())
}

fn property_shape(p: &Property, type_arity: usize) -> Result<(), Unshaped> {
    if !member_name(
        &p.name,
        !p.implements.is_empty() || !p.unclassified_impls.is_empty(),
    ) {
        return Err(Unshaped::MemberName);
    }
    if !std::iter::once(&p.ty)
        .chain(p.parameters.iter().map(|ip| &ip.ty.ty))
        .all(|t| vars_in_range(t, type_arity, 0))
    {
        return Err(Unshaped::TypeParameterIndex);
    }
    if p.drops_optional_modifier {
        return Err(Unshaped::OptionalModifier);
    }
    if p.signature_disagrees_with_accessors {
        return Err(Unshaped::PropertySignature);
    }
    let roles = [(p.has_getter, "get_"), (p.has_setter, "set_")];
    if !accessors_by_convention(&p.name, &roles, &p.accessor_slots) {
        return Err(Unshaped::Accessors);
    }
    Ok(())
}

fn event_shape(e: &Event, type_arity: usize) -> Result<(), Unshaped> {
    if !member_name(
        &e.name,
        !e.implements.is_empty() || !e.unclassified_impls.is_empty(),
    ) {
        return Err(Unshaped::MemberName);
    }
    if !vars_in_range(&e.delegate_type, type_arity, 0) {
        return Err(Unshaped::TypeParameterIndex);
    }
    if e.drops_optional_modifier {
        return Err(Unshaped::OptionalModifier);
    }
    if e.has_fire {
        return Err(Unshaped::FireAccessor);
    }
    if e.signature_disagrees_with_accessors {
        return Err(Unshaped::EventSignature);
    }
    let roles = [(true, "add_"), (true, "remove_")];
    if !accessors_by_convention(&e.name, &roles, &e.accessor_slots) {
        return Err(Unshaped::Accessors);
    }
    Ok(())
}

/// The last segment of an explicit indexer implementation's name
/// (`System.Collections.IList.this[]`), whose accessors are named after
/// `Item` (`System.Collections.IList.get_Item`).
const INDEXER: &str = "this[]";

/// Whether `slots` are one method per present role, in role order, each
/// named `<prefix><name>` — or, for an explicit implementation named
/// `Q.name`, `Q.<prefix><name>` (`Q.<prefix>Item` for `Q.this[]`) — with
/// every `MethodImpl` row it carries that names an accessor by raw name
/// naming one of its own role.
fn accessors_by_convention(name: &str, roles: &[(bool, &str)], slots: &[AccessorSlot]) -> bool {
    let present: Vec<&str> = roles
        .iter()
        .filter(|(present, _)| *present)
        .map(|(_, prefix)| *prefix)
        .collect();
    let (qualifier, simple) = match name.rsplit_once('.') {
        Some((q, INDEXER)) => (Some(q), "Item"),
        Some((q, s)) => (Some(q), s),
        None => (None, name),
    };
    present.len() == slots.len()
        && present.iter().zip(slots).all(|(prefix, slot)| {
            let conventional = format!("{prefix}{simple}");
            let named = match qualifier {
                Some(q) => slot.name == format!("{q}.{conventional}"),
                None => slot.name == conventional,
            };
            named && slot.implements.iter().all(|row| row_of_role(row, prefix))
        })
}

/// Whether a `MethodImpl` row an accessor of role `prefix` carries names an
/// accessor of that role, where it names one by raw name.
fn row_of_role(row: &InterfaceMemberImpl, prefix: &str) -> bool {
    match &row.member {
        ImplementedMember::Unresolved(raw) => raw
            .rsplit('.')
            .next()
            .is_some_and(|last| last.starts_with(prefix)),
        ImplementedMember::Method(_)
        | ImplementedMember::Property(_)
        | ImplementedMember::Event(_) => true,
    }
}

/// Whether `name` is a member's name as a compiler writes it: a plain name;
/// a compiler-generated one, which may name the member it was generated in
/// (`<.cctor>b__0_0`); or, for an explicit implementation, a qualified one
/// whose last segment is plain (or `this[]`, an indexer's) and whose
/// qualifier spells the interface in C#'s type syntax
/// (`N.ISourceBlock<T[]>.M`, `N.IDictionary<K,V>.Add`,
/// `N.IEnumerable<(N.Asymbol,System.Booleanflag)>.GetEnumerator`,
/// `global::N.I.M` from a source generator, `System.Collections.IList.this[]`).
fn member_name(name: &str, explicit: bool) -> bool {
    if plain_name(name) {
        return true;
    }
    if name.starts_with('<') && name.chars().all(|c| c == '.' || is_plain_char(c)) {
        return true;
    }
    explicit
        && name.rsplit_once('.').is_some_and(|(qualifier, last)| {
            (plain_name(last) || last == INDEXER)
                && !qualifier.is_empty()
                && qualifier.chars().all(|c| {
                    matches!(c, '.' | ',' | '[' | ']' | '*' | ':' | '(' | ')') || is_plain_char(c)
                })
        })
}

/// Whether `name` is non-empty and of [`is_plain_char`]s.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(is_plain_char)
}

/// The characters a compiler's names hold: anything but what a
/// documentation ID or a type name reads as structure (`.`, `,`, `#`,
/// `` ` ``, `:`, `@`, `~`, `*`, `&`, `!`, `^`, brackets, braces,
/// parentheses), whitespace and control characters. Compiler-generated
/// names (`<>c__DisplayClass0_0`, `<Name>k__BackingField`, `<G>$8048…`) are
/// of these.
fn is_plain_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(
            c,
            '.' | ','
                | '#'
                | '`'
                | ':'
                | '@'
                | '~'
                | '*'
                | '&'
                | '!'
                | '^'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
        )
}

/// Whether every type parameter `ty` names is declared: a type's (`!i`)
/// below `type_arity`, a method's (`!!i`) below `method_arity`.
fn vars_in_range(ty: &TypeRef, type_arity: usize, method_arity: usize) -> bool {
    match ty {
        TypeRef::Var { index, is_method } => {
            usize::from(*index) < if *is_method { method_arity } else { type_arity }
        }
        TypeRef::Named { type_args, .. } => type_args
            .iter()
            .all(|a| vars_in_range(&a.ty, type_arity, method_arity)),
        TypeRef::Array { element, .. } => vars_in_range(&element.ty, type_arity, method_arity),
        TypeRef::Ptr(Some(inner)) | TypeRef::ByRef { inner, .. } => {
            vars_in_range(inner, type_arity, method_arity)
        }
        TypeRef::Primitive(_) | TypeRef::Ptr(None) => true,
    }
}
