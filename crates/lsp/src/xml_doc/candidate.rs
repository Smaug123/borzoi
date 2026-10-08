//! Which symbol a bare `<inheritdoc/>` inherits from: Roslyn's IDE rule
//! (`ISymbolExtensions.RewriteInheritdocElement`'s `GetCandidateSymbol`),
//! read over the project's [`AssemblyEnv`].
//!
//! The rule, in Roslyn's order:
//!
//! 1. a member that explicitly implements interface members (a `MethodImpl`
//!    row) inherits from the first of them;
//! 2. an override inherits from the member it overrides;
//! 3. an instance constructor inherits from the base type's constructor of
//!    the same signature;
//! 4. any other method, property or event inherits from the first interface
//!    member it implements, implicitly or explicitly, in the containing type's
//!    `AllInterfaces` order;
//! 5. a class inherits from its base type, an interface from its first
//!    declared interface, and a struct, enum or delegate from nothing.
//!
//! Each rule is reproduced from the metadata the way Roslyn's PE importer
//! reads it — the C# overriding and interface-mapping rules
//! (`OverriddenOrHiddenMembersHelpers`, `TypeSymbol
//! .ComputeImplementationForInterfaceMember`, `MakeAllInterfaces`), not the
//! CLR's. Where the env cannot settle the answer exactly the result is
//! [`Candidate::Undecidable`], never a near miss. The recurring traps this
//! avoids (`member-resolution-soundness`):
//!
//! - **Type identity.** Every type reference is bound with
//!   [`AssemblyEnv::il_type_definition`] — by assembly simple name, through
//!   forwarders — never by a first-wins name slot.
//! - **Hiding.** A base level holding the name as another kind of member, a
//!   dropped member of the name, or an internal member of another assembly
//!   (accessible only through `InternalsVisibleTo`, which the env does not
//!   model) is undecidable rather than stepped over.
//! - **Signature comparison.** Signatures are compared in one currency: each
//!   parameter, return and property type rendered as a documentation-comment
//!   type after substituting the declaring type's instantiation into the
//!   context the walk started from. That rendering names types by namespace
//!   and name, as Roslyn's binding through forwarders does, and anything it
//!   cannot tell apart (two candidates at one level) is undecidable.

use borzoi_assembly::doc_id::type_enc;
use borzoi_assembly::{
    Access, AssemblyIdentity, Entity, EntityKind, ImplementedMember, InterfaceMemberImpl, Member,
    NullableType, TypeRef,
};
use borzoi_sema::{AssemblyEnv, EntityHandle, IlTypeDefinition, MemberIndex};

use super::key::DocTarget;

/// How a reached symbol's declaring type is instantiated: its cumulative
/// type parameters, in order, as types in the context of `context` — the type
/// whose own type parameters the [`TypeRef::Var`]s in `args` name.
///
/// The walk starts at a definition (each parameter standing for itself) and
/// composes as it follows base types and interfaces, so a member reached
/// through `class D : B<List<int>>` carries `[List<int>]` for `B`'s `T`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Instantiation {
    pub context: EntityHandle,
    pub args: Vec<TypeRef>,
    /// Each argument's [`type_key`]: which types the arguments are.
    key: Vec<String>,
}

impl Instantiation {
    /// `handle`'s own instantiation: each type parameter stands for itself.
    pub fn definition(env: &AssemblyEnv, handle: EntityHandle) -> Self {
        let arity = env.entity(handle).generic_parameters.len();
        let args: Vec<TypeRef> = (0..arity)
            .map(|i| TypeRef::Var {
                index: u16::try_from(i).unwrap_or(u16::MAX),
                is_method: false,
            })
            .collect();
        Instantiation {
            context: handle,
            key: args.iter().map(type_enc).collect(),
            args,
        }
    }

    /// Which types the arguments are, by bound identity (see [`type_key`]).
    pub fn key(&self) -> &[String] {
        &self.key
    }

    /// `ty`, written in the declaring type's metadata, as a type of
    /// [`Self::context`]. `None` when `ty` names a type parameter the
    /// instantiation does not have (malformed metadata).
    pub fn apply(&self, ty: &TypeRef) -> Option<TypeRef> {
        Some(match ty {
            TypeRef::Var {
                index,
                is_method: false,
            } => self.args.get(usize::from(*index))?.clone(),
            TypeRef::Var {
                is_method: true, ..
            }
            | TypeRef::Primitive(_) => ty.clone(),
            TypeRef::Named {
                assembly,
                namespace,
                name,
                type_args,
                segment_arities,
            } => TypeRef::Named {
                assembly: assembly.clone(),
                namespace: namespace.clone(),
                name: name.clone(),
                type_args: type_args
                    .iter()
                    .map(|a| {
                        Some(NullableType {
                            ty: self.apply(&a.ty)?,
                            nullability: a.nullability,
                        })
                    })
                    .collect::<Option<_>>()?,
                segment_arities: segment_arities.clone(),
            },
            TypeRef::Array {
                element,
                rank,
                sizes,
                lower_bounds,
            } => TypeRef::Array {
                element: Box::new(NullableType {
                    ty: self.apply(&element.ty)?,
                    nullability: element.nullability,
                }),
                rank: *rank,
                sizes: sizes.clone(),
                lower_bounds: lower_bounds.clone(),
            },
            TypeRef::Ptr(inner) => TypeRef::Ptr(match inner {
                Some(inner) => Some(Box::new(self.apply(inner)?)),
                None => None,
            }),
            TypeRef::ByRef { inner, readonly } => TypeRef::ByRef {
                inner: Box::new(self.apply(inner)?),
                readonly: *readonly,
            },
        })
    }

    /// The instantiation of the type `reference` names — a base type or an
    /// interface written in the metadata of the type this instantiates, its
    /// same-module references already qualified.
    fn of_reference(
        &self,
        env: &AssemblyEnv,
        reference: &TypeRef,
    ) -> Result<Instantiation, Undecidable> {
        let TypeRef::Named { type_args, .. } = reference else {
            return Err(Undecidable::MalformedInstantiation);
        };
        let args: Vec<TypeRef> = type_args
            .iter()
            .map(|a| self.apply(&a.ty))
            .collect::<Option<_>>()
            .ok_or(Undecidable::MalformedInstantiation)?;
        let key = args
            .iter()
            .map(|a| type_key(env, self.context, a))
            .collect::<Option<_>>()
            .ok_or(Undecidable::TypeIdentity)?;
        Ok(Instantiation {
            context: self.context,
            args,
            key,
        })
    }
}

/// A type's identity as Roslyn compares types: by the definition each named
/// type binds to ([`AssemblyEnv::il_type_definition`]), not by its name — two
/// assemblies may define the same full name — and, for one no loaded assembly
/// defines, by the full identity of the assembly the reference names, which
/// is what Roslyn's missing-type symbols compare by. `ty` must name its
/// assemblies explicitly (see [`qualify`]); `None` where the binding is
/// ambiguous or uncertain.
pub(crate) fn type_key(env: &AssemblyEnv, from: EntityHandle, ty: &TypeRef) -> Option<String> {
    Some(match ty {
        TypeRef::Primitive(_) | TypeRef::Var { .. } => type_enc(ty),
        TypeRef::Named {
            assembly,
            namespace,
            name,
            type_args,
            segment_arities,
        } => {
            let head = match env.il_type_definition(from, ty) {
                IlTypeDefinition::Resolved(def) => format!("{def:?}"),
                IlTypeDefinition::NotFound => {
                    let assembly = assembly.as_ref()?;
                    format!(
                        "missing {} {:?} {:?} {}.{name} {segment_arities:?}",
                        assembly.name,
                        assembly.version,
                        assembly.public_key_token,
                        namespace.join(".")
                    )
                }
                _ => return None,
            };
            let args = type_args
                .iter()
                .map(|a| type_key(env, from, &a.ty))
                .collect::<Option<Vec<_>>>()?;
            format!("{head}<{}>", args.join(","))
        }
        TypeRef::Array {
            element,
            rank,
            sizes,
            lower_bounds,
        } => format!(
            "{}[{rank};{sizes:?};{lower_bounds:?}]",
            type_key(env, from, &element.ty)?
        ),
        TypeRef::Ptr(Some(inner)) => format!("{}*", type_key(env, from, inner)?),
        TypeRef::Ptr(None) => "void*".to_string(),
        TypeRef::ByRef { inner, .. } => format!("{}&", type_key(env, from, inner)?),
    })
}

/// A symbol a walk has reached, with the instantiation of its declaring type
/// (for an entity, of the entity itself).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reached {
    pub target: DocTarget,
    pub inst: Instantiation,
}

impl Reached {
    /// The definition of `target`, reached directly (a `cref`, or hover).
    pub fn definition(env: &AssemblyEnv, target: DocTarget) -> Self {
        Reached {
            target,
            inst: Instantiation::definition(env, target.owner()),
        }
    }
}

/// What a bare `<inheritdoc/>` on a symbol inherits from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    /// The symbol it inherits from.
    Found(Reached),
    /// Roslyn has no candidate: the `<inheritdoc/>` inherits nothing.
    None(NoCandidate),
    /// The env cannot settle which symbol Roslyn picks.
    Undecidable(Undecidable),
}

/// Why Roslyn has no candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoCandidate {
    /// A struct, enum or delegate: Roslyn inherits nothing for these.
    NotAClassOrInterface,
    /// A class with no base type (`System.Object`), or an interface with no
    /// base interface.
    NoBase,
    /// A field.
    Field,
    /// An override whose overridden member is not virtual, or that overrides
    /// nothing up the chain.
    NothingOverridden,
    /// A constructor with no same-signature base constructor.
    NoBaseConstructor,
    /// A member that implements no interface member.
    ImplementsNothing,
}

/// Why the env cannot settle the candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Undecidable {
    /// A type on the walk is projected in F#'s source view, not as IL.
    NotIl,
    /// A type reference on the walk does not bind.
    TypeUnbound(IlTypeDefinition),
    /// A `MethodImpl` row on the member the projection does not surface (a
    /// covariant-return override, or a row it could not classify).
    UnmodelledMethodImpl,
    /// The member explicitly implements several interface members, or one
    /// the env cannot pin down.
    ExplicitImplementation,
    /// A finalizer, which Roslyn reads as a destructor, not an override.
    Finalizer,
    /// A level of the walk holds the name in a way that may hide or shadow
    /// the match: another kind of member, a nested type, a dropped member,
    /// or an internal member of another assembly.
    Shadowed,
    /// Several members at one level match.
    AmbiguousMatch,
    /// The base-type chain is longer than any real one (a metadata cycle).
    ChainTooLong,
    /// An instantiation names a type parameter its type does not have.
    MalformedInstantiation,
    /// A type in a compared signature or instantiation binds ambiguously or
    /// uncertainly, so which type it is cannot be settled.
    TypeIdentity,
    /// An interface member with a default implementation whose
    /// implementability by a public class member is not modelled.
    DefaultImplementation,
    /// Roslyn's own rule throws here — a constructor of a type with no base
    /// type, whose base it dereferences unchecked — and the exception
    /// abandons the whole expansion.
    RoslynThrows,
}

impl From<Undecidable> for Candidate {
    fn from(why: Undecidable) -> Self {
        Candidate::Undecidable(why)
    }
}

/// More base types than any real hierarchy has.
const MAX_CHAIN: usize = 64;

/// Whether `handle` is read here as the IL a metadata consumer reads: an
/// assembly without an authoritative F# signature, and an IL type kind.
pub fn il_faithful(env: &AssemblyEnv, handle: EntityHandle) -> bool {
    !env.has_authoritative_fsharp_signature(handle)
        && matches!(
            env.entity(handle).kind,
            EntityKind::Class
                | EntityKind::Struct
                | EntityKind::Interface
                | EntityKind::Enum
                | EntityKind::Delegate
        )
}

/// The candidate of `at` (see the module docs).
pub fn candidate(env: &AssemblyEnv, at: &Reached) -> Candidate {
    match at.target {
        DocTarget::Entity(handle) => type_candidate(env, handle, &at.inst),
        DocTarget::Member { parent, idx } => member_candidate(env, parent, idx, &at.inst),
    }
}

/// Bind `reference` (written in `from`'s metadata) and instantiate it.
fn bind(
    env: &AssemblyEnv,
    from: EntityHandle,
    inst: &Instantiation,
    reference: &TypeRef,
) -> Result<(EntityHandle, Instantiation), Undecidable> {
    let def = match env.il_type_definition(from, reference) {
        IlTypeDefinition::Resolved(def) => def,
        other => return Err(Undecidable::TypeUnbound(other)),
    };
    if !il_faithful(env, def) {
        return Err(Undecidable::NotIl);
    }
    // The reference's type arguments leave `from`'s metadata here, so a
    // same-module reference among them must name its module explicitly to
    // keep meaning the same type wherever it is bound later.
    let qualified = qualify(reference, &env.entity(from).assembly);
    let args = inst.of_reference(env, &qualified)?;
    if args.args.len() != env.entity(def).generic_parameters.len() {
        return Err(Undecidable::MalformedInstantiation);
    }
    Ok((def, args))
}

/// `ty` with every same-module reference (`assembly: None`) naming
/// `module`'s assembly explicitly.
fn qualify(ty: &TypeRef, module: &AssemblyIdentity) -> TypeRef {
    let nullable = |a: &NullableType| NullableType {
        ty: qualify(&a.ty, module),
        nullability: a.nullability,
    };
    match ty {
        TypeRef::Named {
            assembly,
            namespace,
            name,
            type_args,
            segment_arities,
        } => TypeRef::Named {
            assembly: Some(assembly.clone().unwrap_or_else(|| module.clone())),
            namespace: namespace.clone(),
            name: name.clone(),
            type_args: type_args.iter().map(nullable).collect(),
            segment_arities: segment_arities.clone(),
        },
        TypeRef::Array {
            element,
            rank,
            sizes,
            lower_bounds,
        } => TypeRef::Array {
            element: Box::new(nullable(element)),
            rank: *rank,
            sizes: sizes.clone(),
            lower_bounds: lower_bounds.clone(),
        },
        TypeRef::Ptr(inner) => TypeRef::Ptr(inner.as_ref().map(|i| Box::new(qualify(i, module)))),
        TypeRef::ByRef { inner, readonly } => TypeRef::ByRef {
            inner: Box::new(qualify(inner, module)),
            readonly: *readonly,
        },
        TypeRef::Primitive(_) | TypeRef::Var { .. } => ty.clone(),
    }
}

fn type_candidate(env: &AssemblyEnv, handle: EntityHandle, inst: &Instantiation) -> Candidate {
    if !il_faithful(env, handle) {
        return Undecidable::NotIl.into();
    }
    let entity = env.entity(handle);
    let reference = match entity.kind {
        EntityKind::Interface => match entity.interfaces.first() {
            Some(first) => first,
            None => return Candidate::None(NoCandidate::NoBase),
        },
        EntityKind::Class if !entity.is_struct => match &entity.base_type {
            Some(base) => base,
            None => return Candidate::None(NoCandidate::NoBase),
        },
        _ => return Candidate::None(NoCandidate::NotAClassOrInterface),
    };
    match bind(env, handle, inst, reference) {
        Ok((def, inst)) => Candidate::Found(Reached {
            target: DocTarget::Entity(def),
            inst,
        }),
        Err(why) => why.into(),
    }
}

fn member_candidate(
    env: &AssemblyEnv,
    parent: EntityHandle,
    idx: MemberIndex,
    inst: &Instantiation,
) -> Candidate {
    if !il_faithful(env, parent) {
        return Undecidable::NotIl.into();
    }
    let member = env.member_at(parent, idx);
    let facts = match MemberFacts::of(member) {
        Some(facts) => facts,
        None => return Candidate::None(NoCandidate::Field),
    };
    if facts.unmodelled_method_impl {
        return Undecidable::UnmodelledMethodImpl.into();
    }
    if !facts.implements.is_empty() {
        return explicit_candidate(env, parent, inst, member, facts.implements);
    }
    let entity = env.entity(parent);
    let is_interface = entity.kind == EntityKind::Interface;
    if let Member::Method(m) = member
        && m.name == "Finalize"
        && m.signature.parameters.is_empty()
        && m.is_virtual
    {
        return Undecidable::Finalizer.into();
    }
    if !is_interface && entity.base_type.is_some() && facts.reuses_a_slot {
        return overridden_candidate(env, parent, inst, member);
    }
    if let Member::Method(m) = member
        && m.is_constructor
    {
        if m.is_static {
            // Roslyn compares against the base type's static constructors,
            // which it never imports (they are private).
            return Candidate::None(NoCandidate::NoBaseConstructor);
        }
        return constructor_candidate(env, parent, inst, member);
    }
    if is_interface {
        // On an interface, Roslyn's "implementation" of a base interface's
        // member is a default implementation, which metadata carries only as
        // a `MethodImpl` row — and a member with rows took rule 1.
        return Candidate::None(NoCandidate::ImplementsNothing);
    }
    interface_candidate(env, parent, inst, member)
}

/// Roslyn's `IsEligibleForAutomaticInheritdoc`: whether an *undocumented*
/// symbol inherits documentation as if its entry were a bare
/// `<inheritdoc/>` — a member that overrides, or that implements some
/// interface member explicitly or implicitly. Never a type, field or
/// constructor.
pub fn inherits_automatically(env: &AssemblyEnv, at: &Reached) -> Result<bool, Undecidable> {
    let DocTarget::Member { parent, idx } = at.target else {
        return Ok(false);
    };
    if !il_faithful(env, parent) {
        return Err(Undecidable::NotIl);
    }
    let member = env.member_at(parent, idx);
    let Some(facts) = MemberFacts::of(member) else {
        return Ok(false);
    };
    if facts.unmodelled_method_impl {
        return Err(Undecidable::UnmodelledMethodImpl);
    }
    if let Member::Method(m) = member
        && m.name == "Finalize"
        && m.signature.parameters.is_empty()
        && m.is_virtual
    {
        return Err(Undecidable::Finalizer);
    }
    let entity = env.entity(parent);
    let is_interface = entity.kind == EntityKind::Interface;
    if !is_interface && entity.base_type.is_some() && facts.reuses_a_slot {
        return Ok(true);
    }
    if !facts.implements.is_empty() {
        return Ok(true);
    }
    if is_interface {
        // As in `member_candidate`: an interface member implements another
        // only through a `MethodImpl` row, and it has none.
        return Ok(false);
    }
    match interface_candidate(env, parent, &at.inst, member) {
        Candidate::Found(_) => Ok(true),
        Candidate::None(_) => Ok(false),
        Candidate::Undecidable(why) => Err(why),
    }
}

/// The facts of a member the rules read, uniformly over kinds.
struct MemberFacts<'a> {
    implements: &'a [InterfaceMemberImpl],
    /// Some accessor (or the method) reuses a base vtable slot: Roslyn's
    /// `IsOverride` for a PE member with a base type.
    reuses_a_slot: bool,
    /// A `MethodImpl` row on it is not surfaced as an interface member.
    unmodelled_method_impl: bool,
}

impl<'a> MemberFacts<'a> {
    fn of(member: &'a Member) -> Option<Self> {
        Some(match member {
            Member::Method(m) => MemberFacts {
                implements: &m.implements,
                reuses_a_slot: m.is_virtual && !m.is_newslot,
                unmodelled_method_impl: m.has_other_method_impl || !m.unclassified_impls.is_empty(),
            },
            Member::Property(p) => MemberFacts {
                implements: &p.implements,
                reuses_a_slot: p
                    .accessor_slots
                    .iter()
                    .any(|s| s.is_virtual && !s.is_newslot),
                unmodelled_method_impl: p.accessor_slots.iter().any(|s| s.has_other_method_impl)
                    || !p.unclassified_impls.is_empty(),
            },
            Member::Event(e) => MemberFacts {
                implements: &e.implements,
                reuses_a_slot: e
                    .accessor_slots
                    .iter()
                    .any(|s| s.is_virtual && !s.is_newslot),
                unmodelled_method_impl: e.accessor_slots.iter().any(|s| s.has_other_method_impl)
                    || !e.unclassified_impls.is_empty(),
            },
            Member::Field(_) => return None,
        })
    }
}

/// A member's signature in the comparison currency (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Signature {
    kind: Kind,
    is_static: bool,
    generic_arity: usize,
    /// Each parameter's type, with `@` for a byref and, when the comparison
    /// distinguishes them, `out` for an out parameter.
    parameters: Vec<String>,
    /// The return type (a property's or event's type).
    returns: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Method,
    Property,
    Event,
    Field,
}

fn kind(member: &Member) -> Kind {
    match member {
        Member::Method(_) => Kind::Method,
        Member::Property(_) => Kind::Property,
        Member::Event(_) => Kind::Event,
        Member::Field(_) => Kind::Field,
    }
}

pub(crate) fn member_name(member: &Member) -> &str {
    match member {
        Member::Method(m) => &m.name,
        Member::Property(p) => &p.name,
        Member::Event(e) => &e.name,
        Member::Field(f) => &f.name,
    }
}

fn member_access(member: &Member) -> Access {
    match member {
        Member::Method(m) => m.access,
        Member::Property(p) => p.access,
        Member::Event(e) => e.access,
        Member::Field(f) => f.access,
    }
}

fn is_static(member: &Member) -> bool {
    match member {
        Member::Method(m) => m.is_static,
        Member::Property(p) => p.is_static,
        Member::Event(e) => e.is_static,
        Member::Field(f) => f.is_static,
    }
}

/// How a comparison treats byref parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Byrefs {
    /// Roslyn's override comparer: `ref` and `out` differ.
    RefOutDistinct,
    /// Roslyn's implicit-implementation comparer: any byref is `@`.
    RefOutSame,
    /// `IsSameSignature` (base constructors): types only.
    Ignored,
}

/// `member`'s signature (it is declared on `declaring`, instantiated by
/// `inst`), each type by its bound identity ([`type_key`]). Undecidable where
/// a type names a parameter the instantiation lacks or binds uncertainly.
fn signature(
    env: &AssemblyEnv,
    declaring: EntityHandle,
    member: &Member,
    inst: &Instantiation,
    byrefs: Byrefs,
) -> Result<Signature, Undecidable> {
    signature_of(env, declaring, member, inst, byrefs).ok_or(Undecidable::TypeIdentity)
}

/// Whether `member`'s signature is `want`. A signature that cannot be
/// settled is undecidable, not a mismatch: treating it as one would step
/// past a member that may be the match.
fn same_signature(
    env: &AssemblyEnv,
    declaring: EntityHandle,
    member: &Member,
    inst: &Instantiation,
    byrefs: Byrefs,
    want: &Signature,
) -> Result<bool, Undecidable> {
    Ok(signature(env, declaring, member, inst, byrefs)? == *want)
}

fn signature_of(
    env: &AssemblyEnv,
    declaring: EntityHandle,
    member: &Member,
    inst: &Instantiation,
    byrefs: Byrefs,
) -> Option<Signature> {
    let module = &env.entity(declaring).assembly;
    let enc = |ty: &TypeRef| {
        inst.apply(&qualify(ty, module))
            .and_then(|t| type_key(env, inst.context, &t))
    };
    Some(match member {
        Member::Method(m) => Signature {
            kind: Kind::Method,
            is_static: m.is_static,
            generic_arity: m.generic_parameters.len(),
            parameters: m
                .signature
                .parameters
                .iter()
                .map(|p| {
                    let mut s = enc(&p.ty)?;
                    match byrefs {
                        Byrefs::Ignored => {}
                        Byrefs::RefOutSame if p.is_byref => s.push('@'),
                        Byrefs::RefOutDistinct if p.is_byref => {
                            s.push('@');
                            if p.is_out {
                                s.push_str("out");
                            }
                        }
                        _ => {}
                    }
                    Some(s)
                })
                .collect::<Option<_>>()?,
            returns: enc(&m.signature.return_type)?,
        },
        Member::Property(p) => Signature {
            kind: Kind::Property,
            is_static: p.is_static,
            generic_arity: 0,
            parameters: p
                .parameters
                .iter()
                .map(|ip| enc(&ip.ty.ty))
                .collect::<Option<_>>()?,
            returns: enc(&p.ty)?,
        },
        Member::Event(e) => Signature {
            kind: Kind::Event,
            is_static: e.is_static,
            generic_arity: 0,
            parameters: Vec::new(),
            returns: enc(&e.delegate_type)?,
        },
        Member::Field(f) => Signature {
            kind: Kind::Field,
            is_static: f.is_static,
            generic_arity: 0,
            parameters: Vec::new(),
            returns: enc(&f.ty)?,
        },
    })
}

/// `handle`'s members, with their indices.
fn members(
    env: &AssemblyEnv,
    handle: EntityHandle,
) -> impl Iterator<Item = (MemberIndex, &Member)> {
    env.member_indices(handle)
        .map(move |idx| (idx, env.member_at(handle, idx)))
}

/// Rule 1: the first interface member a `MethodImpl` row names. Roslyn's
/// order over several rows is not modelled, so only a single row commits.
fn explicit_candidate(
    env: &AssemblyEnv,
    parent: EntityHandle,
    inst: &Instantiation,
    member: &Member,
    implements: &[InterfaceMemberImpl],
) -> Candidate {
    if implements
        .iter()
        .any(|i| matches!(i.member, ImplementedMember::Unresolved(_)))
    {
        return unresolved_explicit_candidate(env, parent, inst, member, implements);
    }
    let [only] = implements else {
        return Undecidable::ExplicitImplementation.into();
    };
    let (iface, iface_inst) = match bind(env, parent, inst, &only.interface) {
        Ok(bound) => bound,
        Err(why) => return why.into(),
    };
    let (wanted_kind, wanted_name) = match &only.member {
        ImplementedMember::Method(name) => (Kind::Method, name),
        ImplementedMember::Property(name) => (Kind::Property, name),
        ImplementedMember::Event(name) => (Kind::Event, name),
        ImplementedMember::Unresolved(_) => return Undecidable::ExplicitImplementation.into(),
    };
    explicit_target(
        env,
        (parent, member, inst),
        iface,
        &iface_inst,
        wanted_kind,
        wanted_name,
    )
}

/// Rule 1 when the implemented interface is in another assembly: its
/// `MethodSemantics` are out of the projection's reach, so each `MethodImpl`
/// row names a raw interface *method* (`Dispose`, `get_Item`). A method's row
/// names that method; a property's or event's rows name its accessors, which
/// C# and VB name `get_P`/`set_P` and `add_E`/`remove_E`. The convention is
/// trusted only where nothing else on the interface could carry the raw
/// name — no method named like an accessor, no property or event a method's
/// name could be the accessor of — and only when every accessor of the member
/// has its row.
fn unresolved_explicit_candidate(
    env: &AssemblyEnv,
    parent: EntityHandle,
    inst: &Instantiation,
    member: &Member,
    implements: &[InterfaceMemberImpl],
) -> Candidate {
    let raws: Vec<&str> = implements
        .iter()
        .map(|i| match &i.member {
            ImplementedMember::Unresolved(raw) => Some(raw.as_str()),
            _ => None,
        })
        .collect::<Option<_>>()
        .unwrap_or_default();
    if raws.len() != implements.len() || raws.is_empty() {
        return Undecidable::ExplicitImplementation.into();
    }
    let mut bound = Vec::new();
    for i in implements {
        match bind(env, parent, inst, &i.interface) {
            Ok(b) => bound.push(b),
            Err(why) => return why.into(),
        }
    }
    let (iface, iface_inst) = bound[0].clone();
    if bound
        .iter()
        .any(|(d, a)| *d != iface || a.key() != iface_inst.key())
    {
        return Undecidable::ExplicitImplementation.into();
    }
    let prefixed = |prefixes: &[&str]| -> Option<&str> {
        let names: Vec<&str> = raws
            .iter()
            .map(|raw| prefixes.iter().find_map(|p| raw.strip_prefix(p)))
            .collect::<Option<_>>()?;
        names.windows(2).all(|w| w[0] == w[1]).then(|| names[0])
    };
    let (wanted_kind, wanted_name, accessors): (Kind, &str, Vec<String>) = match member {
        Member::Method(_) => match raws.as_slice() {
            [raw] => (Kind::Method, raw, Vec::new()),
            _ => return Undecidable::ExplicitImplementation.into(),
        },
        Member::Property(p) => {
            let Some(name) = prefixed(&["get_", "set_"]) else {
                return Undecidable::ExplicitImplementation.into();
            };
            let mut want = Vec::new();
            if p.has_getter {
                want.push(format!("get_{name}"));
            }
            if p.has_setter {
                want.push(format!("set_{name}"));
            }
            (Kind::Property, name, want)
        }
        Member::Event(e) => {
            let Some(name) = prefixed(&["add_", "remove_", "raise_"]) else {
                return Undecidable::ExplicitImplementation.into();
            };
            let mut want = vec![format!("add_{name}"), format!("remove_{name}")];
            if e.has_fire {
                want.push(format!("raise_{name}"));
            }
            (Kind::Event, name, want)
        }
        Member::Field(_) => return Undecidable::ExplicitImplementation.into(),
    };
    if wanted_kind != Kind::Method {
        let mut have: Vec<String> = raws.iter().map(|r| r.to_string()).collect();
        have.sort();
        let mut want = accessors.clone();
        want.sort();
        if have != want {
            return Undecidable::ExplicitImplementation.into();
        }
    }
    // Nothing else on the interface may carry the raw names.
    let lookalike = members(env, iface).any(|(_, m)| match (wanted_kind, m) {
        (Kind::Method, Member::Property(_) | Member::Event(_)) => {
            may_name(
                &ImplementedMember::Unresolved(wanted_name.to_string()),
                member_name(m),
            ) && member_name(m) != wanted_name
        }
        (Kind::Property | Kind::Event, Member::Method(x)) => accessors.contains(&x.name),
        _ => false,
    });
    if lookalike {
        return Undecidable::ExplicitImplementation.into();
    }
    explicit_target(
        env,
        (parent, member, inst),
        iface,
        &iface_inst,
        wanted_kind,
        wanted_name,
    )
}

/// The one member of `iface` of kind `wanted_kind`, named `wanted_name`,
/// whose signature (under `iface_inst`) is `member`'s (under `inst`).
fn explicit_target(
    env: &AssemblyEnv,
    (parent, member, inst): (EntityHandle, &Member, &Instantiation),
    iface: EntityHandle,
    iface_inst: &Instantiation,
    wanted_kind: Kind,
    wanted_name: &str,
) -> Candidate {
    if wanted_kind != kind(member) {
        // A method standing in for an accessor (or the reverse): Roslyn's
        // candidate is then an accessor method, documented nowhere.
        return Undecidable::ExplicitImplementation.into();
    }
    let mine = match signature(env, parent, member, inst, Byrefs::RefOutSame) {
        Ok(s) => s,
        Err(why) => return why.into(),
    };
    if has_skipped(env, iface, wanted_name) {
        return Undecidable::Shadowed.into();
    }
    let mut found = Vec::new();
    for (idx, m) in members(env, iface) {
        if kind(m) != wanted_kind || member_name(m) != wanted_name {
            continue;
        }
        match same_signature(env, iface, m, iface_inst, Byrefs::RefOutSame, &mine) {
            Ok(true) => found.push(idx),
            Ok(false) => {}
            Err(why) => return why.into(),
        }
    }
    match found.as_slice() {
        [idx] => Candidate::Found(Reached {
            target: DocTarget::Member {
                parent: iface,
                idx: *idx,
            },
            inst: iface_inst.clone(),
        }),
        _ => Undecidable::ExplicitImplementation.into(),
    }
}

/// Whether an implemented-member entry may name the interface member called
/// `name`: by name, or — for an entry whose kind the projection could not
/// read — as one of its accessors by the `get_`/`set_`/`add_`/`remove_`/
/// `raise_` convention. Over-matching is safe where this is used: it only
/// ever declines.
fn may_name(implemented: &ImplementedMember, name: &str) -> bool {
    match implemented {
        ImplementedMember::Method(n)
        | ImplementedMember::Property(n)
        | ImplementedMember::Event(n) => n == name,
        ImplementedMember::Unresolved(raw) => {
            raw == name
                || ["get_", "set_", "add_", "remove_", "raise_"]
                    .iter()
                    .any(|p| raw.strip_prefix(p) == Some(name))
        }
    }
}

/// Whether `handle` dropped a member named `name` while projecting.
fn has_skipped(env: &AssemblyEnv, handle: EntityHandle, name: &str) -> bool {
    env.entity(handle)
        .skipped_members
        .iter()
        .any(|s| s.name == name || s.name.ends_with(&format!("_{name}")))
}

/// Whether `handle` has a nested type named `name`.
fn has_nested_named(env: &AssemblyEnv, handle: EntityHandle, name: &str) -> bool {
    env.children(handle)
        .iter()
        .any(|&c| env.entity(c).name == name)
}

/// Whether two handles come from the same loaded DLL.
fn same_assembly(env: &AssemblyEnv, a: EntityHandle, b: EntityHandle) -> Option<bool> {
    Some(env.assembly_path(a)? == env.assembly_path(b)?)
}

/// The base type of `level`, bound and instantiated; `Ok(None)` at the root.
fn base_of(
    env: &AssemblyEnv,
    level: EntityHandle,
    inst: &Instantiation,
) -> Result<Option<(EntityHandle, Instantiation)>, Undecidable> {
    match &env.entity(level).base_type {
        None => Ok(None),
        Some(base) => bind(env, level, inst, base).map(Some),
    }
}

/// Rule 2: the member an override overrides — the nearest base level with a
/// same-kind member of the same name and signature (Roslyn's
/// `FindOverriddenOrHiddenMembers` for a metadata member).
fn overridden_candidate(
    env: &AssemblyEnv,
    parent: EntityHandle,
    inst: &Instantiation,
    member: &Member,
) -> Candidate {
    let name = member_name(member);
    let mine = match signature(env, parent, member, inst, Byrefs::RefOutDistinct) {
        Ok(s) => s,
        Err(why) => return why.into(),
    };
    let mut level = (parent, inst.clone());
    for _ in 0..MAX_CHAIN {
        level = match base_of(env, level.0, &level.1) {
            Ok(Some(next)) => next,
            Ok(None) => return Candidate::None(NoCandidate::NothingOverridden),
            Err(why) => return why.into(),
        };
        let (def, def_inst) = &level;
        if has_skipped(env, *def, name) || has_nested_named(env, *def, name) {
            return Undecidable::Shadowed.into();
        }
        let mut matches = Vec::new();
        for (idx, other) in members(env, *def) {
            if member_name(other) != name {
                continue;
            }
            match accessible_from(env, other, *def, parent) {
                Accessible::No => continue,
                Accessible::Unknown => return Undecidable::Shadowed.into(),
                Accessible::Yes => {}
            }
            if kind(other) != kind(member) {
                return Undecidable::Shadowed.into();
            }
            match same_signature(env, *def, other, def_inst, Byrefs::RefOutDistinct, &mine) {
                Ok(true) => matches.push((idx, other)),
                Ok(false) => {}
                Err(why) => return why.into(),
            }
        }
        match matches.as_slice() {
            [] => continue,
            [(idx, other)] => {
                return if is_overridable(other) {
                    Candidate::Found(Reached {
                        target: DocTarget::Member {
                            parent: *def,
                            idx: *idx,
                        },
                        inst: def_inst.clone(),
                    })
                } else {
                    Candidate::None(NoCandidate::NothingOverridden)
                };
            }
            _ => return Undecidable::AmbiguousMatch.into(),
        }
    }
    Undecidable::ChainTooLong.into()
}

/// Roslyn's `GetOverriddenMember` keeps a match only if it is virtual,
/// abstract or an override.
/// Whether an interface member is one a class member can implement.
enum Implementable {
    Yes,
    /// Not virtual, or sealed: a private or sealed default implementation.
    No,
    /// Virtual but not public, or with accessors that disagree: whether a
    /// public member implements it turns on rules not modelled here.
    Unknown,
}

fn implementable(member: &Member) -> Implementable {
    let open = |virtual_: bool, final_: bool| virtual_ && !final_;
    let verdict = |opens: &[bool]| match (opens.iter().all(|o| *o), opens.iter().any(|o| *o)) {
        (true, true) => Implementable::Yes,
        (_, false) => Implementable::No,
        _ => Implementable::Unknown,
    };
    let structural = match member {
        Member::Method(m) => verdict(&[open(m.is_virtual, m.is_final)]),
        Member::Property(p) => verdict(
            &p.accessor_slots
                .iter()
                .map(|s| open(s.is_virtual, s.is_final))
                .collect::<Vec<_>>(),
        ),
        Member::Event(e) => verdict(
            &e.accessor_slots
                .iter()
                .map(|s| open(s.is_virtual, s.is_final))
                .collect::<Vec<_>>(),
        ),
        Member::Field(_) => Implementable::No,
    };
    match structural {
        Implementable::Yes if member_access(member) != Access::Public => Implementable::Unknown,
        other => other,
    }
}

fn is_overridable(member: &Member) -> bool {
    match member {
        Member::Method(m) => m.is_virtual,
        Member::Property(p) => p.accessor_slots.iter().any(|s| s.is_virtual),
        Member::Event(e) => e.accessor_slots.iter().any(|s| s.is_virtual),
        Member::Field(_) => false,
    }
}

enum Accessible {
    Yes,
    No,
    Unknown,
}

/// Whether `member` of base level `def` is accessible from `derived` the
/// way Roslyn's `IsOverriddenSymbolAccessible` decides it. Internal access
/// across assemblies turns on `InternalsVisibleTo`, which the env does not
/// model.
fn accessible_from(
    env: &AssemblyEnv,
    member: &Member,
    def: EntityHandle,
    derived: EntityHandle,
) -> Accessible {
    match member_access(member) {
        Access::Public | Access::Protected | Access::ProtectedOrInternal => Accessible::Yes,
        Access::Private => Accessible::No,
        Access::Internal | Access::ProtectedAndInternal => match same_assembly(env, def, derived) {
            Some(true) => Accessible::Yes,
            _ => Accessible::Unknown,
        },
    }
}

/// Rule 3: the base type's constructor of the same signature (Roslyn's
/// `IsSameSignature`: parameter types and static-ness, not ref-kinds).
fn constructor_candidate(
    env: &AssemblyEnv,
    parent: EntityHandle,
    inst: &Instantiation,
    member: &Member,
) -> Candidate {
    let (base, base_inst) = match base_of(env, parent, inst) {
        Ok(Some(base)) => base,
        // Roslyn dereferences the base type without a check here, abandoning
        // the whole expansion.
        Ok(None) => return Undecidable::RoslynThrows.into(),
        Err(why) => return why.into(),
    };
    let mine = match signature(env, parent, member, inst, Byrefs::Ignored) {
        Ok(s) => s,
        Err(why) => return why.into(),
    };
    if has_skipped(env, base, ".ctor") {
        return Undecidable::Shadowed.into();
    }
    let mut found = Vec::new();
    for (idx, m) in members(env, base) {
        if !matches!(m, Member::Method(c) if c.is_constructor && !c.is_static && imported_non_virtual(c.access))
        {
            continue;
        }
        match same_signature(env, base, m, &base_inst, Byrefs::Ignored, &mine) {
            Ok(true) => found.push(idx),
            Ok(false) => {}
            Err(why) => return why.into(),
        }
    }
    match found.as_slice() {
        [] => Candidate::None(NoCandidate::NoBaseConstructor),
        [idx] => Candidate::Found(Reached {
            target: DocTarget::Member {
                parent: base,
                idx: *idx,
            },
            inst: base_inst,
        }),
        _ => Undecidable::AmbiguousMatch.into(),
    }
}

/// Whether Roslyn imports a non-virtual method of this access from a
/// referenced assembly (`MetadataImportOptions.Public`).
fn imported_non_virtual(access: Access) -> bool {
    !matches!(access, Access::Private | Access::Internal)
}

/// An interface of a type's closure, bound and instantiated.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Iface {
    def: EntityHandle,
    inst: Instantiation,
}

impl Iface {
    fn key(&self) -> (EntityHandle, Vec<String>) {
        (self.def, self.inst.key().to_vec())
    }
}

/// The interfaces `handle` declares (its `InterfaceImpl` rows), bound.
fn declared_interfaces(
    env: &AssemblyEnv,
    handle: EntityHandle,
    inst: &Instantiation,
) -> Result<Vec<Iface>, Undecidable> {
    env.entity(handle)
        .interfaces
        .iter()
        .map(|i| bind(env, handle, inst, i).map(|(def, inst)| Iface { def, inst }))
        .collect()
}

/// Bound on the size of an interface closure, against crafted metadata whose
/// generic interfaces instantiate themselves ever larger.
const MAX_INTERFACES: usize = 1024;

/// Roslyn's `MakeAllInterfaces`: every interface of `handle`, its base
/// types' and their base interfaces', in Roslyn's (topological) order.
fn all_interfaces(
    env: &AssemblyEnv,
    handle: EntityHandle,
    inst: &Instantiation,
) -> Result<Vec<Iface>, Undecidable> {
    fn add_all(
        env: &AssemblyEnv,
        iface: Iface,
        visited: &mut Vec<(EntityHandle, Vec<String>)>,
        result: &mut Vec<Iface>,
    ) -> Result<(), Undecidable> {
        if visited.contains(&iface.key()) {
            return Ok(());
        }
        if visited.len() >= MAX_INTERFACES {
            return Err(Undecidable::ChainTooLong);
        }
        visited.push(iface.key());
        let bases = declared_interfaces(env, iface.def, &iface.inst)?;
        for base in bases.into_iter().rev() {
            add_all(env, base, visited, result)?;
        }
        result.push(iface);
        Ok(())
    }
    let mut visited = Vec::new();
    let mut result = Vec::new();
    let mut level = Some((handle, inst.clone()));
    let mut steps = 0;
    while let Some((def, def_inst)) = level {
        steps += 1;
        if steps > MAX_CHAIN {
            return Err(Undecidable::ChainTooLong);
        }
        for iface in declared_interfaces(env, def, &def_inst)?.into_iter().rev() {
            add_all(env, iface, &mut visited, &mut result)?;
        }
        level = base_of(env, def, &def_inst)?;
    }
    result.reverse();
    Ok(result)
}

/// Roslyn's `InterfacesAndTheirBaseInterfaces` of `handle`: the interfaces it
/// declares and theirs, transitively — the set whose members the type's own
/// members may implement implicitly.
fn declaring_closure(
    env: &AssemblyEnv,
    handle: EntityHandle,
    inst: &Instantiation,
) -> Result<Vec<(EntityHandle, Vec<String>)>, Undecidable> {
    let mut seen: Vec<(EntityHandle, Vec<String>)> = Vec::new();
    let mut stack = declared_interfaces(env, handle, inst)?;
    while let Some(iface) = stack.pop() {
        if seen.contains(&iface.key()) {
            continue;
        }
        if seen.len() >= MAX_INTERFACES {
            return Err(Undecidable::ChainTooLong);
        }
        seen.push(iface.key());
        stack.extend(declared_interfaces(env, iface.def, &iface.inst)?);
    }
    Ok(seen)
}

/// Rule 4: the first interface member `member` (declared on `parent`, and
/// explicitly implementing nothing) implements, in `AllInterfaces` order.
fn interface_candidate(
    env: &AssemblyEnv,
    parent: EntityHandle,
    inst: &Instantiation,
    member: &Member,
) -> Candidate {
    let name = member_name(member);
    let interfaces = match all_interfaces(env, parent, inst) {
        Ok(all) => all,
        Err(why) => return why.into(),
    };
    let declaring = match declaring_closure(env, parent, inst) {
        Ok(closure) => closure,
        Err(why) => return why.into(),
    };
    // A row the projection could not place might be an explicit
    // implementation of the very member in question.
    if members(env, parent).any(|(_, m)| match m {
        Member::Method(x) => !x.unclassified_impls.is_empty(),
        Member::Property(x) => !x.unclassified_impls.is_empty(),
        Member::Event(x) => !x.unclassified_impls.is_empty(),
        Member::Field(_) => false,
    }) {
        return Undecidable::UnmodelledMethodImpl.into();
    }
    if has_skipped(env, parent, name) {
        return Undecidable::Shadowed.into();
    }
    for iface in &interfaces {
        // Roslyn looks for an implicit implementation on a type only from
        // the first type (walking up from the containing type) that declares
        // the interface; `member`'s own type is the first one asked.
        if !declaring.contains(&iface.key()) {
            continue;
        }
        if has_skipped(env, iface.def, name) {
            return Undecidable::Shadowed.into();
        }
        let mut implemented = Vec::new();
        for (idx, im) in members(env, iface.def) {
            // A static interface member is implemented implicitly only by a
            // type from source; from metadata, only through `MethodImpl`.
            if member_name(im) != name || kind(im) != kind(member) || is_static(im) {
                continue;
            }
            // Only an abstract or virtual, unsealed member is implemented at
            // all (Roslyn's `IsImplementableInterfaceMember`); a private or
            // sealed default implementation is not, whatever shares its name.
            match implementable(im) {
                Implementable::No => continue,
                Implementable::Unknown => return Undecidable::DefaultImplementation.into(),
                Implementable::Yes => {}
            }
            let theirs = match signature(env, iface.def, im, &iface.inst, Byrefs::RefOutSame) {
                Ok(s) => s,
                Err(why) => return why.into(),
            };
            // The implicit implementation: the first public member of the
            // type with the interface member's name, kind, static-ness and
            // signature. Unless that is `member`, `member` implements nothing
            // here, whatever else does.
            let mut implicit: Vec<&Member> = Vec::new();
            for (_, x) in members(env, parent) {
                if member_name(x) != name
                    || kind(x) != kind(im)
                    || member_access(x) != Access::Public
                    || is_static(x)
                {
                    continue;
                }
                match same_signature(env, parent, x, inst, Byrefs::RefOutSame, &theirs) {
                    Ok(true) => implicit.push(x),
                    Ok(false) => {}
                    Err(why) => return why.into(),
                }
            }
            if !implicit.iter().any(|x| std::ptr::eq(*x, member)) {
                continue;
            }
            if implicit.len() > 1 {
                // Which one Roslyn's member order makes first is not modelled.
                return Undecidable::AmbiguousMatch.into();
            }
            // An explicit implementation on the type takes precedence. The
            // model names the implemented member, not its signature, so an
            // overloaded name cannot be told apart.
            let explicitly = members(env, parent).any(|(_, x)| {
                let implements = match x {
                    Member::Method(x) => &x.implements,
                    Member::Property(x) => &x.implements,
                    Member::Event(x) => &x.implements,
                    Member::Field(_) => return false,
                };
                implements.iter().any(|i| {
                    may_name(&i.member, name)
                        && bind(env, parent, inst, &i.interface)
                            .is_ok_and(|(def, inst)| Iface { def, inst }.key() == iface.key())
                })
            });
            if explicitly {
                return Undecidable::ExplicitImplementation.into();
            }
            implemented.push(idx);
        }
        match implemented.as_slice() {
            [] => {}
            [idx] => {
                return Candidate::Found(Reached {
                    target: DocTarget::Member {
                        parent: iface.def,
                        idx: *idx,
                    },
                    inst: iface.inst.clone(),
                });
            }
            _ => return Undecidable::AmbiguousMatch.into(),
        }
    }
    Candidate::None(NoCandidate::ImplementsNothing)
}

/// The cumulative type-parameter names of `handle` as Roslyn's
/// `GetAllTypeParameters` lists them: each from the level that introduces it,
/// outermost first.
pub fn type_parameter_names(env: &AssemblyEnv, handle: EntityHandle) -> Option<Vec<String>> {
    let chain = env.enclosing_chain_from_root(handle)?;
    let mut names = Vec::new();
    for link in chain {
        let own = &env.entity(link).generic_parameters;
        names.extend(own.iter().skip(names.len()).map(|p| p.name.clone()));
    }
    Some(names)
}

/// The entity a [`DocTarget`] is declared on.
pub fn declaring_entity(env: &AssemblyEnv, target: DocTarget) -> &Entity {
    env.entity(target.owner())
}
