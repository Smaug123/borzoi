//! [`compiler_shape`]'s accessor rules, checked
//! as a metamorphic property over real assemblies.
//!
//! C# writes a property's getter as `(index…) → T`, its setter as
//! `(index…, T) → void`, and an event's adder and remover as
//! `(D) → void`, for the property's type `T` and the event's delegate `D`.
//! A consumer certifying only compiler-shaped metadata must therefore reject
//! a type whose accessor disagrees in any one position. That is a statement
//! about two projections of one program, so it needs no hand-written case:
//! retype one accessor position in every property or event of a real
//! assembly ([`Position`]) and re-project. Each bindable property or event
//! that was compiler-shaped must now be dropped or unshaped.
//!
//! It is judged member by member (the type cut down to that member), so one
//! member's rejection cannot hide another's acceptance, and each position is
//! retyped in a pass of its own, so a check on one position cannot stand in
//! for a missing check on another.

use crate::reader::{Image, Method, ModifiedType, Primitive, RetType, TypeSig};
use crate::shape::{bindable, compiler_shape};
use crate::{Ecma335Assembly, EcmaView, Entity, ImportError, Member};

/// One accessor position, retyped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// A getter's return type.
    GetterReturn,
    /// A getter's first index parameter (indexers only).
    GetterIndex,
    /// A setter's value parameter (its last).
    SetterValue,
    /// A setter's first index parameter (indexers only).
    SetterIndex,
    /// A setter's return, made non-void.
    SetterReturn,
    /// An adder's parameter.
    AdderParameter,
    /// A remover's parameter.
    RemoverParameter,
    /// An adder's return, made non-void.
    AdderReturn,
}

pub const POSITIONS: [Position; 8] = [
    Position::GetterReturn,
    Position::GetterIndex,
    Position::SetterValue,
    Position::SetterIndex,
    Position::SetterReturn,
    Position::AdderParameter,
    Position::RemoverParameter,
    Position::AdderReturn,
];

/// What one probe saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessorOutcome {
    /// Bindable, compiler-shaped properties and events whose accessor at the
    /// position was retyped: the members the property is about. A probe
    /// that retyped none proves nothing.
    pub retyped: usize,
    /// Each retyped member that stayed compiler-shaped. Empty iff the
    /// property held.
    pub findings: Vec<String>,
}

/// Retype `position` in every property or event of the assembly in `bytes`
/// and report the members [`compiler_shape`] still accepts.
pub fn retyped_accessors_are_unshaped(
    bytes: &[u8],
    position: Position,
) -> Result<AccessorOutcome, ImportError> {
    let baseline_asm = Ecma335Assembly::parse(bytes)?;
    let baseline = flatten(&baseline_asm.enumerate_type_defs()?);
    let mut retyped_asm = baseline_asm.clone();
    retype(retyped_asm.image_mut(), position);
    let retyped = flatten(&retyped_asm.enumerate_type_defs()?);
    let mut outcome = AccessorOutcome {
        retyped: 0,
        findings: Vec::new(),
    };
    for before in &baseline {
        let Some(after) = retyped.iter().find(|e| same_type(e, before)) else {
            continue;
        };
        for (i, member) in before.members.iter().enumerate().filter(|(_, m)| {
            matches!(m, Member::Property(_) | Member::Event(_))
                && bindable(m)
                && has_position(m, position)
                && compiler_shape(&alone(before, m)).is_ok()
        }) {
            outcome.retyped += 1;
            // Overloads share a name (two indexers `Item`): match the same
            // occurrence of the name.
            let namesake =
                |m: &&Member| member_name(m) == member_name(member) && same_kind(m, member);
            let occurrence = before.members[..i].iter().filter(namesake).count();
            let Some(now) = after.members.iter().filter(namesake).nth(occurrence) else {
                continue;
            };
            if compiler_shape(&alone(after, now)).is_ok() {
                outcome.findings.push(format!(
                    "{}.{}: still compiler-shaped with its {position:?} retyped",
                    qualified(before),
                    member_name(member)
                ));
            }
        }
    }
    Ok(outcome)
}

/// Whether the projected `member` has an accessor with `position`, which
/// [`retype`] therefore retyped.
fn has_position(member: &Member, position: Position) -> bool {
    match (member, position) {
        (Member::Property(p), Position::GetterReturn) => p.has_getter,
        (Member::Property(p), Position::GetterIndex) => p.has_getter && !p.parameters.is_empty(),
        (Member::Property(p), Position::SetterValue | Position::SetterReturn) => p.has_setter,
        (Member::Property(p), Position::SetterIndex) => p.has_setter && !p.parameters.is_empty(),
        (
            Member::Event(_),
            Position::AdderParameter | Position::RemoverParameter | Position::AdderReturn,
        ) => true,
        _ => false,
    }
}

/// Retype `position` in every property's or event's accessor that has it.
fn retype(image: &mut Image, position: Position) {
    for td in &mut image.type_defs {
        let mut targets: Vec<u32> = Vec::new();
        for p in &td.properties {
            let accessor = match position {
                Position::GetterReturn | Position::GetterIndex => p.getter,
                Position::SetterValue | Position::SetterIndex | Position::SetterReturn => p.setter,
                _ => None,
            };
            if let Some(id) = accessor {
                targets.push(id.0);
            }
        }
        for e in &td.events {
            let accessor = match position {
                Position::AdderParameter | Position::AdderReturn => e.add,
                Position::RemoverParameter => e.remove,
                _ => None,
            };
            if let Some(id) = accessor {
                targets.push(id.0);
            }
        }
        for id in targets {
            if let Some(m) = td.methods.get_mut(id as usize) {
                retype_method(m, position);
            }
        }
    }
}

/// Retype `position` of `m`'s signature; whether it had that position.
fn retype_method(m: &mut Method, position: Position) -> bool {
    let Ok(sig) = &mut m.signature else {
        return false;
    };
    let other = |t: &ModifiedType| {
        ModifiedType::plain(TypeSig::Primitive(
            if t.ty == TypeSig::Primitive(Primitive::Int32) {
                Primitive::String
            } else {
                Primitive::Int32
            },
        ))
    };
    match position {
        Position::GetterReturn => match &mut sig.return_type {
            RetType::Type(t) => {
                *t = other(t);
                true
            }
            RetType::Void(_) => false,
        },
        Position::SetterReturn | Position::AdderReturn => {
            sig.return_type =
                RetType::Type(ModifiedType::plain(TypeSig::Primitive(Primitive::Int32)));
            true
        }
        Position::GetterIndex | Position::SetterIndex => {
            let index_count = match position {
                Position::SetterIndex => sig.parameters.len().saturating_sub(1),
                _ => sig.parameters.len(),
            };
            match sig.parameters.first_mut() {
                Some(p) if index_count > 0 => {
                    p.ty = other(&p.ty);
                    true
                }
                _ => false,
            }
        }
        Position::SetterValue | Position::AdderParameter | Position::RemoverParameter => {
            match sig.parameters.last_mut() {
                Some(p) => {
                    p.ty = other(&p.ty);
                    true
                }
                None => false,
            }
        }
    }
}

fn qualified(e: &Entity) -> String {
    format!("{}.{}", e.namespace.join("."), e.name)
}

fn flatten(entities: &[Entity]) -> Vec<Entity> {
    fn walk(e: &Entity, prefix: Option<&str>, out: &mut Vec<Entity>) {
        let mut named = e.clone();
        if let Some(p) = prefix {
            named.namespace = Vec::new();
            named.name = format!("{p}+{}", e.name);
        }
        let key = qualified(&named);
        out.push(named);
        for n in &e.nested_types {
            walk(n, Some(key.trim_start_matches('.')), out);
        }
    }
    let mut out = Vec::new();
    for e in entities {
        walk(e, None, &mut out);
    }
    out
}

fn same_type(a: &Entity, b: &Entity) -> bool {
    a.namespace == b.namespace && a.name == b.name
}

/// `e` cut down to `member`, which [`compiler_shape`] then judges alone.
fn alone(e: &Entity, member: &Member) -> Entity {
    let mut one = e.clone();
    one.members = vec![member.clone()];
    one.nested_types = Vec::new();
    one
}

fn member_name(m: &Member) -> &str {
    match m {
        Member::Method(m) => &m.name,
        Member::Field(f) => &f.name,
        Member::Property(p) => &p.name,
        Member::Event(e) => &e.name,
    }
}

fn same_kind(a: &Member, b: &Member) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}
