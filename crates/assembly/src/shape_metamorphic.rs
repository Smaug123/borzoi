//! [`compiler_shape`]'s accessor and name rules, checked as metamorphic
//! properties over real assemblies.
//!
//! **Accessors.**
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
//!
//! **Names.** A documentation ID reads a dot in a type's name as nesting, a
//! backtick suffix as an arity, and a `#` or backtick in a member's name as
//! syntax. Rename every type, or every ordinary method, one such way
//! ([`Rename`]) and re-project: each type that was compiler-shaped must now be
//! rejected, and for exactly the renamed part. The projection is where this
//! can fail unseen — a name it normalises (an arity suffix it strips) never
//! reaches the predicate's character rules.
//!
//! **Encodings.** C# spells a special type by its element type (`string`,
//! never `class System.String`), constructs a generic type with exactly its
//! arity's arguments, writes an array as the vector or as a zero-based
//! unsized shape of rank two or more, and gives an instance property instance
//! accessors.
//! Re-encode every ordinary method's signature, or every instance property's
//! getter, one other way ([`Encoding`]) and re-project: each member that was
//! compiler-shaped must now be rejected for that reason, judged alone.

use crate::reader::{
    Image, Method, ModifiedType, NamedKind, Param, Primitive, RetType, TypeName, TypeRefId,
    TypeScope, TypeSig,
};
use crate::shape::{Unshaped, bindable, compiler_shape};
use crate::{Ecma335Assembly, EcmaView, Entity, ImportError, Member, TypeRef as ModelTypeRef};

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
    for (before, enclosing) in &baseline {
        let enclosing = enclosing.as_ref();
        let Some((after, _)) = retyped.iter().find(|(e, _)| same_type(e, before)) else {
            continue;
        };
        let in_scope = |m: &Member| matches!(m, Member::Property(_) | Member::Event(_));
        let touched = |m: &Member| has_position(m, position);
        let paired = match counterparts(&before.members, &after.members, &in_scope, &touched) {
            Ok(paired) => paired,
            Err(lost) => {
                outcome
                    .findings
                    .push(format!("{}: {lost}", qualified(before)));
                continue;
            }
        };
        for (member, now) in before.members.iter().zip(paired).filter(|(m, _)| {
            in_scope(m)
                && bindable(m)
                && touched(m)
                && compiler_shape(&alone(before, m), enclosing).is_ok()
        }) {
            outcome.retyped += 1;
            // Dropped by the projector: never certified.
            let Some(now) = now else {
                continue;
            };
            if compiler_shape(&alone(after, now), enclosing).is_ok() {
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

/// Each member of `before` that `in_scope` admits paired with its
/// re-projection among `after`, or `None` where the projector dropped it.
///
/// Only the members `touched` says were rewritten may differ, so within one
/// group of namesakes (overloads, two indexers `Item`) an untouched member
/// survives verbatim and in order. A touched member is the next survivor,
/// unless that survivor is the next untouched member verbatim: then it was
/// dropped. An untouched member that does not survive verbatim means
/// `touched` is wrong, and the alignment is lost: `Err`, never a guess.
fn counterparts<'a>(
    before: &[Member],
    after: &'a [Member],
    in_scope: &dyn Fn(&Member) -> bool,
    touched: &dyn Fn(&Member) -> bool,
) -> Result<Vec<Option<&'a Member>>, String> {
    let mut paired = vec![None; before.len()];
    let mut grouped = vec![false; before.len()];
    for lead in 0..before.len() {
        if grouped[lead] || !in_scope(&before[lead]) {
            continue;
        }
        let namesake = |m: &Member| {
            member_name(m) == member_name(&before[lead]) && same_kind(m, &before[lead])
        };
        let group: Vec<usize> = (lead..before.len())
            .filter(|&i| namesake(&before[i]))
            .collect();
        let mut survivors = after.iter().filter(|m| namesake(m)).peekable();
        for (k, &i) in group.iter().enumerate() {
            grouped[i] = true;
            if touched(&before[i]) {
                let next_untouched = group[k + 1..]
                    .iter()
                    .map(|&n| &before[n])
                    .find(|m| !touched(m));
                if survivors.peek().is_some_and(|&s| Some(s) != next_untouched) {
                    paired[i] = survivors.next();
                }
            } else {
                let survivor = survivors.next();
                if survivor != Some(&before[i]) {
                    return Err(format!(
                        "{}: an untouched member did not survive verbatim",
                        member_name(&before[i])
                    ));
                }
                paired[i] = survivor;
            }
        }
        if survivors.next().is_some() {
            return Err(format!(
                "{}: more re-projected namesakes than before",
                member_name(&before[lead])
            ));
        }
    }
    Ok(paired)
}

fn qualified(e: &Entity) -> String {
    format!("{}.{}", e.namespace.join("."), e.name)
}

/// Every type, nested ones under their enclosing types' names, each with
/// the type it is nested in.
fn flatten(entities: &[Entity]) -> Vec<(Entity, Option<Entity>)> {
    fn walk(
        e: &Entity,
        prefix: Option<&str>,
        enclosing: Option<&Entity>,
        out: &mut Vec<(Entity, Option<Entity>)>,
    ) {
        let mut named = e.clone();
        if let Some(p) = prefix {
            named.namespace = Vec::new();
            named.name = format!("{p}+{}", e.name);
        }
        let key = qualified(&named);
        named.nested_types = Vec::new();
        out.push((named, enclosing.map(alone_shell)));
        for n in &e.nested_types {
            walk(n, Some(key.trim_start_matches('.')), Some(e), out);
        }
    }
    let mut out = Vec::new();
    for e in entities {
        walk(e, None, None, &mut out);
    }
    out
}

/// `e` without members or nested types: all an enclosing type contributes
/// to its nested type's verdict is its type parameters.
fn alone_shell(e: &Entity) -> Entity {
    let mut shell = e.clone();
    shell.members = Vec::new();
    shell.nested_types = Vec::new();
    shell
}

/// Whether `a` and `b` are one type: by name and by arity, which the
/// projected name has stripped (`ValueTuple` and ``ValueTuple`1`` are both
/// named `ValueTuple`).
fn same_type(a: &Entity, b: &Entity) -> bool {
    a.namespace == b.namespace
        && a.name == b.name
        && a.generic_parameters.len() == b.generic_parameters.len()
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

/// One way to rename types or methods so that a documentation ID reads the
/// name otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rename {
    /// A dot inside every type's name (`Foo` → `F.oo`).
    DotInTypeName,
    /// Every type's arity suffix one more than its own arity (`Foo` →
    /// ``Foo`1``, ``List`1`` → ``List`2``).
    WrongAritySuffix,
    // Neither applies to a core library: renaming `System.Object` or a
    // marker attribute the projector reads (`IsByRefLikeAttribute`, …)
    // changes how other types project, which is another property than this
    // one.
    /// A `#` inside every ordinary method's name.
    HashInMethodName,
    /// A backtick inside every ordinary method's name.
    BacktickInMethodName,
}

pub const RENAMES: [Rename; 4] = [
    Rename::DotInTypeName,
    Rename::WrongAritySuffix,
    Rename::HashInMethodName,
    Rename::BacktickInMethodName,
];

/// Apply `rename` to the assembly in `bytes` and report each type that was
/// compiler-shaped and is not now rejected for the renamed part. A type
/// rename must make every such type [`Unshaped::TypeName`]; a method rename
/// every such type with a bindable ordinary method [`Unshaped::MemberName`].
pub fn renamed_names_are_unshaped(
    bytes: &[u8],
    rename: Rename,
) -> Result<AccessorOutcome, ImportError> {
    let baseline_asm = Ecma335Assembly::parse(bytes)?;
    let baseline = flatten(&baseline_asm.enumerate_type_defs()?);
    let renames_types = matches!(rename, Rename::DotInTypeName | Rename::WrongAritySuffix);
    if renames_types
        && baseline
            .iter()
            .any(|(e, _)| e.namespace == ["System"] && e.name == "Object")
    {
        return Ok(AccessorOutcome {
            retyped: 0,
            findings: Vec::new(),
        });
    }
    let mut renamed_asm = baseline_asm.clone();
    apply(renamed_asm.image_mut(), rename);
    let renamed = flatten(&renamed_asm.enumerate_type_defs()?);
    let mut outcome = AccessorOutcome {
        retyped: 0,
        findings: Vec::new(),
    };
    if baseline.len() != renamed.len() {
        outcome.findings.push(format!(
            "{rename:?}: the type count moved: {} -> {}",
            baseline.len(),
            renamed.len()
        ));
        return Ok(outcome);
    }
    let want = match rename {
        Rename::DotInTypeName | Rename::WrongAritySuffix => Unshaped::TypeName,
        Rename::HashInMethodName | Rename::BacktickInMethodName => Unshaped::MemberName,
    };
    for ((before, enclosing), (after, renamed_enclosing)) in baseline.iter().zip(&renamed) {
        if compiler_shape(before, enclosing.as_ref()).is_err() {
            continue;
        }
        let affected = match want {
            Unshaped::MemberName => before.members.iter().any(|m| {
                matches!(m, Member::Method(method) if !method.is_constructor) && bindable(m)
            }),
            _ => true,
        };
        if !affected {
            continue;
        }
        outcome.retyped += 1;
        let verdict = compiler_shape(after, renamed_enclosing.as_ref());
        if verdict != Err(want) {
            outcome.findings.push(format!(
                "{} (now {}.{}): {verdict:?} after {rename:?}, want {want:?}",
                qualified(before),
                after.namespace.join("."),
                after.name
            ));
        }
    }
    Ok(outcome)
}

fn apply(image: &mut Image, rename: Rename) {
    // Row 0 is `<Module>`, which no consumer reads as a type.
    let arities: Vec<usize> = (0..image.type_defs.len())
        .map(|i| {
            let td = &image.type_defs[i];
            let outer = td
                .enclosing
                .map_or(0, |e| image.type_defs[e.0 as usize].generic_params.len());
            td.generic_params.len().saturating_sub(outer)
        })
        .collect();
    for (i, td) in image.type_defs.iter_mut().enumerate().skip(1) {
        match rename {
            Rename::DotInTypeName => {
                let name = &td.name.name;
                let at = name.chars().next().map_or(0, char::len_utf8);
                td.name.name = format!("{}.{}", &name[..at], &name[at..]);
            }
            Rename::WrongAritySuffix => {
                let bare = crate::ecma335_assembly::strip_arity(&td.name.name).to_string();
                td.name.name = format!("{bare}`{}", arities[i] + 1);
            }
            Rename::HashInMethodName | Rename::BacktickInMethodName => {
                let accessors: Vec<u32> = td
                    .properties
                    .iter()
                    .flat_map(|p| [p.getter, p.setter])
                    .chain(td.events.iter().flat_map(|e| [e.add, e.remove, e.raise]))
                    .flatten()
                    .map(|id| id.0)
                    .collect();
                let mark = if rename == Rename::HashInMethodName {
                    '#'
                } else {
                    '`'
                };
                for (k, m) in td.methods.iter_mut().enumerate() {
                    if accessors.contains(&(k as u32)) || m.name.starts_with('.') {
                        continue;
                    }
                    let at = m.name.chars().next().map_or(0, char::len_utf8);
                    m.name = format!("{}{mark}{}", &m.name[..at], &m.name[at..]);
                }
            }
        }
    }
}

/// One way to encode a signature that C# does not write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Every `string` in an ordinary method's signature spelled
    /// `class System.String`.
    SpecialByReference,
    /// Every generic instantiation in an ordinary method's signature given
    /// one more argument (`int`).
    ExtraTypeArgument,
    /// Every instance property's getter made static, taking an `int` first,
    /// as F# writes an instance property over a static accessor.
    StaticGetterOverInstanceProperty,
    /// Every vector in an ordinary method's signature written as the rank-1
    /// array shape `[0:]`.
    RankOneArray,
    /// Every array in an ordinary method's signature given the lower bound 3
    /// in its first dimension.
    NonZeroLowerBound,
    /// Every array in an ordinary method's signature given the size 2 in its
    /// first dimension.
    DeclaredSize,
}

pub const ENCODINGS: [Encoding; 6] = [
    Encoding::SpecialByReference,
    Encoding::ExtraTypeArgument,
    Encoding::StaticGetterOverInstanceProperty,
    Encoding::RankOneArray,
    Encoding::NonZeroLowerBound,
    Encoding::DeclaredSize,
];

/// Re-encode the assembly in `bytes` by `encoding` and report each member
/// that was compiler-shaped and touched, and is not now rejected for the
/// encoding ([`Unshaped::SignatureEncoding`], or for a getter
/// [`Unshaped::PropertySignature`]), judged alone.
pub fn reencoded_signatures_are_unshaped(
    bytes: &[u8],
    encoding: Encoding,
) -> Result<AccessorOutcome, ImportError> {
    let baseline_asm = Ecma335Assembly::parse(bytes)?;
    let baseline = flatten(&baseline_asm.enumerate_type_defs()?);
    let mut outcome = AccessorOutcome {
        retyped: 0,
        findings: Vec::new(),
    };
    let mut encoded_asm = baseline_asm.clone();
    if !encode(encoded_asm.image_mut(), encoding) {
        return Ok(outcome);
    }
    let encoded = flatten(&encoded_asm.enumerate_type_defs()?);
    let want = match encoding {
        Encoding::StaticGetterOverInstanceProperty => Unshaped::PropertySignature,
        _ => Unshaped::SignatureEncoding,
    };
    for (before, enclosing) in &baseline {
        let enclosing = enclosing.as_ref();
        let Some((after, _)) = encoded.iter().find(|(e, _)| same_type(e, before)) else {
            continue;
        };
        let in_scope = |m: &Member| match encoding {
            Encoding::StaticGetterOverInstanceProperty => matches!(m, Member::Property(_)),
            _ => matches!(m, Member::Method(_)),
        };
        let touched = |m: &Member| touched_by(m, encoding);
        let paired = match counterparts(&before.members, &after.members, &in_scope, &touched) {
            Ok(paired) => paired,
            Err(lost) => {
                outcome
                    .findings
                    .push(format!("{}: {lost}", qualified(before)));
                continue;
            }
        };
        for (member, now) in before.members.iter().zip(paired).filter(|(m, _)| {
            in_scope(m)
                && bindable(m)
                && touched(m)
                && compiler_shape(&alone(before, m), enclosing).is_ok()
        }) {
            outcome.retyped += 1;
            // Dropped by the projector: never certified.
            let Some(now) = now else {
                continue;
            };
            let verdict = compiler_shape(&alone(after, now), enclosing);
            if verdict != Err(want) {
                outcome.findings.push(format!(
                    "{}.{}: {verdict:?} after {encoding:?}, want {want:?}",
                    qualified(before),
                    member_name(member)
                ));
            }
        }
    }
    Ok(outcome)
}

/// Whether [`encode`] re-encoded the projected `member`.
fn touched_by(member: &Member, encoding: Encoding) -> bool {
    fn mentions(ty: &ModelTypeRef, hit: &dyn Fn(&ModelTypeRef) -> bool) -> bool {
        hit(ty)
            || match ty {
                ModelTypeRef::Named { type_args, .. } => {
                    type_args.iter().any(|a| mentions(&a.ty, hit))
                }
                ModelTypeRef::Array { element, .. } => mentions(&element.ty, hit),
                ModelTypeRef::Ptr(Some(inner)) | ModelTypeRef::ByRef { inner, .. } => {
                    mentions(inner, hit)
                }
                _ => false,
            }
    }
    let hit: &dyn Fn(&ModelTypeRef) -> bool = match encoding {
        Encoding::SpecialByReference => {
            &|t| matches!(t, ModelTypeRef::Primitive(crate::Primitive::String))
        }
        Encoding::ExtraTypeArgument => {
            &|t| matches!(t, ModelTypeRef::Named { type_args, .. } if !type_args.is_empty())
        }
        Encoding::StaticGetterOverInstanceProperty => {
            return matches!(member, Member::Property(p) if !p.is_static && p.has_getter);
        }
        Encoding::RankOneArray => &|t| matches!(t, ModelTypeRef::Array { rank: 1, .. }),
        Encoding::NonZeroLowerBound | Encoding::DeclaredSize => {
            &|t| matches!(t, ModelTypeRef::Array { .. })
        }
    };
    match member {
        Member::Method(m) if !m.is_constructor => std::iter::once(&m.signature.return_type)
            .chain(m.signature.parameters.iter().map(|p| &p.ty))
            .any(|t| mentions(t, hit)),
        _ => false,
    }
}

/// Re-encode by `encoding`; whether the image allowed it (a core library
/// has no `TypeRef` to `System.Object` whose scope a `System.String`
/// reference could share).
fn encode(image: &mut Image, encoding: Encoding) -> bool {
    let string_ref = match encoding {
        Encoding::SpecialByReference => {
            let Some(scope) = image
                .type_refs
                .iter()
                .find(|r| r.name.namespace == "System" && r.name.name == "Object")
                .and_then(|r| r.scope.ok())
            else {
                return false;
            };
            image.type_refs.push(crate::reader::TypeRefRow {
                name: TypeName {
                    namespace: "System".into(),
                    name: "String".into(),
                },
                scope: Ok(scope),
            });
            Some(TypeRefId(
                u32::try_from(image.type_refs.len() - 1).unwrap_or(u32::MAX),
            ))
        }
        _ => None,
    };
    for td in &mut image.type_defs {
        let accessors: Vec<u32> = td
            .properties
            .iter()
            .flat_map(|p| [p.getter, p.setter])
            .chain(td.events.iter().flat_map(|e| [e.add, e.remove, e.raise]))
            .flatten()
            .map(|id| id.0)
            .collect();
        match encoding {
            Encoding::StaticGetterOverInstanceProperty => {
                let getters: Vec<u32> = td
                    .properties
                    .iter()
                    .filter(|p| p.has_this == Some(true))
                    .filter_map(|p| p.getter.map(|g| g.0))
                    .collect();
                for g in getters {
                    let Some(m) = td.methods.get_mut(g as usize) else {
                        continue;
                    };
                    let Ok(sig) = &mut m.signature else {
                        continue;
                    };
                    m.is_static = true;
                    sig.has_this = false;
                    sig.parameters.insert(
                        0,
                        Param {
                            name: None,
                            ty: ModifiedType::plain(TypeSig::Primitive(Primitive::Int32)),
                            is_in: false,
                            is_out: false,
                            optional: false,
                            default_value: None,
                            attributes: Vec::new(),
                        },
                    );
                }
            }
            Encoding::SpecialByReference
            | Encoding::ExtraTypeArgument
            | Encoding::RankOneArray
            | Encoding::NonZeroLowerBound
            | Encoding::DeclaredSize => {
                for (k, m) in td.methods.iter_mut().enumerate() {
                    // A constructor is left alone: re-encoding an attribute's
                    // (`NullableAttribute(byte[])`) would break the decoding of
                    // every use of it, dropping members the encoding never
                    // touched.
                    if accessors.contains(&(k as u32)) || m.name == ".ctor" || m.name == ".cctor" {
                        continue;
                    }
                    reencode_method(m, encoding, string_ref);
                }
            }
        }
    }
    true
}

fn reencode_method(m: &mut Method, encoding: Encoding, string_ref: Option<TypeRefId>) {
    let Ok(sig) = &mut m.signature else {
        return;
    };
    if let RetType::Type(t) = &mut sig.return_type {
        reencode(t, encoding, string_ref);
    }
    for p in &mut sig.parameters {
        reencode(&mut p.ty, encoding, string_ref);
    }
}

fn reencode(t: &mut ModifiedType, encoding: Encoding, string_ref: Option<TypeRefId>) {
    match &mut t.ty {
        TypeSig::Primitive(Primitive::String) if encoding == Encoding::SpecialByReference => {
            if let Some(id) = string_ref {
                t.ty = TypeSig::Named {
                    kind: Some(NamedKind::Class),
                    scope: TypeScope::Reference(id),
                };
            }
        }
        TypeSig::Generic { args, .. } => {
            for a in args.iter_mut() {
                reencode(a, encoding, string_ref);
            }
            if encoding == Encoding::ExtraTypeArgument {
                args.push(ModifiedType::plain(TypeSig::Primitive(Primitive::Int32)));
            }
        }
        TypeSig::SzArray(e) => {
            reencode(e, encoding, string_ref);
            let shape = match encoding {
                Encoding::RankOneArray => Some((Vec::new(), vec![0])),
                Encoding::NonZeroLowerBound => Some((Vec::new(), vec![3])),
                Encoding::DeclaredSize => Some((vec![2], vec![0])),
                _ => None,
            };
            if let Some((sizes, lower_bounds)) = shape {
                let element =
                    std::mem::replace(e, Box::new(ModifiedType::plain(TypeSig::TypedByRef)));
                t.ty = TypeSig::Array {
                    element,
                    rank: 1,
                    sizes,
                    lower_bounds,
                };
            }
        }
        TypeSig::ByRef(e) | TypeSig::Ptr(Some(e)) => reencode(e, encoding, string_ref),
        TypeSig::Array {
            element,
            sizes,
            lower_bounds,
            ..
        } => {
            reencode(element, encoding, string_ref);
            match encoding {
                Encoding::NonZeroLowerBound => {
                    lower_bounds.resize(lower_bounds.len().max(1), 0);
                    lower_bounds[0] = 3;
                }
                Encoding::DeclaredSize => {
                    sizes.resize(sizes.len().max(1), 0);
                    sizes[0] = 2;
                }
                _ => {}
            }
        }
        _ => {}
    }
}
