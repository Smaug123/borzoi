//! The F# type of a referenced F# module function or value, read off its IL
//! signature.
//!
//! An F# module function compiles to a static method (a value to a static
//! property, or a generic method for a type function) whose IL signature erases
//! four things its F# type has: how the parameters group into curried
//! arguments, the F#-only constraints on its type parameters, any unit of
//! measure, and (in the return position) `unit`. The signature pickle supplies
//! the first three per method — [`MethodLike::arg_group_count`], each
//! parameter's [`FSharpConstraints`](borzoi_assembly::FSharpConstraints), and
//! [`MethodLike::val_facts`] — and the fourth is a fixed rule. With them the IL
//! signature *is* the F# type, so [`module_function_scheme`] rebuilds that type
//! as a scheme over the method's type parameters; anything the facts cannot
//! vouch for is a [`FunctionTyGap`], and the caller declines (D5).
//!
//! A measure need not appear in the val's pickled type to have been erased: a
//! function over `type Dist = float<m>` pickles the name `Dist` only, compiles
//! to a `float64` parameter, and FCS types it `float<m> -> …`. So the pickle
//! lists every named type the val's type applies
//! ([`ValMeasures::NoDirectMeasure`]), and each must be cleared through the
//! env's abbreviation chase ([`AssemblyEnv::head_hides_no_measure`]) — in
//! whichever assembly declares it — before the IL is trusted.
//!
//! A use F# rejects is a gap too, since FCS then types the use as its error
//! type rather than by the signature: an `[<Obsolete(_, true)>]` (`^` types as
//! `System.Object`), an error-level `[<CompilerMessage>]`, a
//! `[<RequiresExplicitTypeArguments>]` written without them, or a val another
//! assembly cannot access.
//!
//! The types it carries over are deliberately few: primitives, the function's
//! own type parameters, F# function types, reference tuples (`System.Tuple`
//! of two to seven elements), vectors, `unit`, and non-generic named types
//! the env resolves to one entity that F# names as the IL does. A generic
//! named type is a gap: FSharp.Core's own (`'T list`, `'T option`) are
//! renamed, which `Ty`'s one path cannot carry (FCS renders the compiled
//! `FSharpList`, hover must show `list`), and bridging the rest soundly
//! needs the checks `Gen::applied_annotation_ty` makes of an annotation head.
//! Bridging every named type, unchecked, was measured on the corpus sample to
//! add no agreement over this set, while other blockers dominate. A
//! nullable-reference annotation is a gap too: `Ty` cannot carry F#'s `| null`.

use borzoi_assembly::{
    Access, AssemblyIdentity, Augmentation, EntityKind, MethodLike, NamedTypeHead, Nullability,
    ParamDefault, Primitive, TypeRef, ValFacts, ValMeasures,
};

use crate::assembly_env::{AssemblyEnv, EntityHandle};
use crate::infer::constrained_parameter;
use crate::member_ty::primitive_to_ty;
use crate::ty::Ty;

/// FSharp.Core's public-key token.
const FSHARP_CORE_PUBLIC_KEY_TOKEN: [u8; 8] = [0xb0, 0x3f, 0x5f, 0x7f, 0x11, 0xd5, 0x0a, 0x3a];

/// F#'s `unit`, as FCS names it.
const UNIT: &str = "Microsoft.FSharp.Core.Unit";

/// Whether `id` is FSharp.Core: its name *and* its public-key token, so that
/// an assembly that merely calls itself `FSharp.Core` is not believed.
fn is_fsharp_core(id: &AssemblyIdentity) -> bool {
    id.name == "FSharp.Core" && id.public_key_token == Some(FSHARP_CORE_PUBLIC_KEY_TOKEN)
}

/// Why a method's IL signature cannot be read as its F# type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FunctionTyGap {
    /// Not a plain function or value of an F# module: a member of a type, a
    /// constructor, an extension or augmentation member.
    NotAFunction,
    /// The F# compiler rejects a use: `[<Obsolete(_, true)>]`, an error-level
    /// `[<CompilerMessage>]`, `[<RequiresExplicitTypeArguments>]`, or a val
    /// another assembly cannot access.
    Rejected,
    /// An F# method whose val the signature pickle did not describe, so neither
    /// its use-checked attributes nor its units of measure are known.
    Unread,
    /// A type parameter is constrained — by IL, by an F#-only constraint, or by
    /// one the pickle was not read for.
    Constrained,
    /// The F# type mentions a unit of measure the IL erased, or names a type
    /// not proven free of one.
    Measures,
    /// The parameters do not map onto argument groups in a way that is read:
    /// several groups of which some are tupled, a group count the pickle did
    /// not supply, a byref, an optional or a `params` parameter.
    Groups,
    /// A parameter or the return has a type this bridge does not carry.
    Type,
}

/// The F# type of `method`, a function or value of the F# module `handle` in
/// `env` — as a scheme: curried over its argument groups, with each of its
/// type parameters as the [`Ty::Param`] of its index, ready for instantiation.
///
/// The argument groups are the val's pickled ones
/// ([`MethodLike::arg_group_count`]), and they map onto the IL parameters in
/// exactly four ways, which together are the shapes read:
///
/// - **No group**: a module *value* (`let x = …`, or a type function such as
///   `List.empty`, which compiles to a generic method). It has no parameter,
///   and its type is the return type.
/// - **One parameter per group**: a curried function, one domain per
///   parameter. F# pickles an empty argument group only as a function's
///   *only* one, and any other `()` group is one unit-typed parameter
///   (`SynInfo.AdjustArgsForUnitElimination` in the F# compiler), so with as
///   many parameters as groups no group is empty and none holds two.
/// - **One group and no parameter**: that sole empty group, so `let f () = …`
///   is `unit -> …`.
/// - **One group and several parameters**: a tupled group,
///   `let f (a, b) = …`, whose one domain is the tuple of the parameters.
///
/// Any other count (several groups, some of them tupled) needs the size of
/// each group, which is not read, and declines.
///
/// A function whose F# result is a function returns that function type in
/// the IL, so `let f x = fun y -> …` (one group) reads as `'a -> 'b -> 'c`,
/// which is its F# type.
pub(crate) fn module_function_scheme(
    env: &AssemblyEnv,
    handle: EntityHandle,
    method: &MethodLike,
) -> Result<Ty, FunctionTyGap> {
    let module = env.entity(handle);
    if module.kind != EntityKind::Module
        || !method.is_static
        || method.is_constructor
        || method.is_extension_method
        || method.augmentation != Augmentation::No
    {
        return Err(FunctionTyGap::NotAFunction);
    }
    // Another assembly's non-public val is not accessible here (FS1094), and
    // an obsolete-as-error one is rejected at the use.
    if method.access != Access::Public || method.obsolete.as_ref().is_some_and(|o| o.is_error) {
        return Err(FunctionTyGap::Rejected);
    }
    match &method.val_facts {
        ValFacts::NotFSharp => {}
        ValFacts::Unknowable => return Err(FunctionTyGap::Unread),
        ValFacts::Read(facts) => {
            // Any error-level message, `nameof`'s suppressed 3501 included:
            // FCS types that one by its own rule, not by its signature.
            if facts.compiler_message.as_ref().is_some_and(|m| m.is_error)
                || facts.requires_explicit_type_arguments
            {
                return Err(FunctionTyGap::Rejected);
            }
            match &facts.measures {
                ValMeasures::Measured => return Err(FunctionTyGap::Measures),
                ValMeasures::NoDirectMeasure { heads } => {
                    if !heads.iter().all(|head| {
                        is_fsharp_core_array_head(head) || env.head_hides_no_measure(handle, head)
                    }) {
                        return Err(FunctionTyGap::Measures);
                    }
                }
            }
        }
    }
    if method.generic_parameters.iter().any(constrained_parameter) {
        return Err(FunctionTyGap::Constrained);
    }
    let params = &method.signature.parameters;
    if params
        .iter()
        .any(|p| p.is_byref || p.is_out || p.is_param_array || p.default != ParamDefault::None)
    {
        return Err(FunctionTyGap::Groups);
    }
    if method.signature.return_nullability == Nullability::Annotated
        || params
            .iter()
            .any(|p| p.nullability == Nullability::Annotated)
    {
        return Err(FunctionTyGap::Type);
    }
    let bridge = Bridge {
        env,
        declaring: &module.assembly,
        typars: method.generic_parameters.len(),
    };
    let ret = match &method.signature.return_type {
        // An F# function returning `unit` compiles to a `void` method.
        TypeRef::Primitive(Primitive::Void) => Ty::named(UNIT),
        ret => bridge.ty(ret)?,
    };
    let params = params
        .iter()
        .map(|p| bridge.ty(&p.ty))
        .collect::<Result<Vec<_>, _>>()?;
    let domains = curried_domains(method, params)?;
    Ok(domains.into_iter().rev().fold(ret, |ret, arg| Ty::Fun {
        arg: Box::new(arg),
        ret: Box::new(ret),
    }))
}

/// The curried domains of `method`'s F# type, from its bridged IL parameter
/// types `params` and its pickled argument-group count, in the four shapes
/// [`module_function_scheme`] lists; any other count is a
/// [`FunctionTyGap::Groups`]. This is the one place the groups are mapped
/// onto the parameters, so reading each group's own size, which would admit
/// several groups of which some are tupled, changes only this function.
fn curried_domains(method: &MethodLike, mut params: Vec<Ty>) -> Result<Vec<Ty>, FunctionTyGap> {
    let value = method.is_module_value_binding;
    Ok(match (method.arg_group_count, params.len()) {
        (Some(0), 0) if value => Vec::new(),
        (Some(1), 0) if !value => vec![Ty::named(UNIT)],
        (Some(1), n) if n >= 2 && !value => vec![Ty::Tuple(std::mem::take(&mut params))],
        (Some(groups), n) if groups == n && !value => params,
        _ => return Err(FunctionTyGap::Groups),
    })
}

/// Whether a pickled head is FSharp.Core's vector tycon (`'T[]`), which F#
/// declares in the pickle alone, with no IL type to resolve: it carries no
/// measure of its own, and its element is a type argument the pickle's scan
/// has already covered.
fn is_fsharp_core_array_head(head: &NamedTypeHead) -> bool {
    head.ccu.as_deref() == Some("FSharp.Core")
        && head
            .path
            .iter()
            .map(String::as_str)
            .eq(["Microsoft", "FSharp", "Core", "[]`1"])
}

/// Whether a reference names one of the assemblies that hold, or forward to,
/// the core library's `System` types: a `System.Tuple` from anywhere else is
/// not F#'s tuple.
fn is_core_library(id: &AssemblyIdentity) -> bool {
    matches!(
        id.name.as_str(),
        "System.Runtime" | "netstandard" | "mscorlib" | "System.Private.CoreLib"
    )
}

/// Whether a namespace is FSharp.Core's `Microsoft.FSharp.Core`.
fn is_fsharp_core_namespace(namespace: &[String]) -> bool {
    namespace
        .iter()
        .map(String::as_str)
        .eq(["Microsoft", "FSharp", "Core"])
}

/// The per-signature context of the `TypeRef → Ty` conversion: whose
/// assembly a same-assembly reference names, and how many method type
/// parameters a [`TypeRef::Var`] may index.
struct Bridge<'a> {
    env: &'a AssemblyEnv,
    declaring: &'a AssemblyIdentity,
    typars: usize,
}

impl Bridge<'_> {
    fn ty(&self, ty: &TypeRef) -> Result<Ty, FunctionTyGap> {
        match ty {
            TypeRef::Primitive(Primitive::Void) => Err(FunctionTyGap::Type),
            TypeRef::Primitive(p) => Ok(primitive_to_ty(*p)),
            TypeRef::Var {
                index,
                is_method: true,
            } if usize::from(*index) < self.typars => Ok(Ty::Param(u32::from(*index))),
            // FSharp.Core's own `FSharpFunc<_, _>` and `Unit`, by their
            // resolution scope: a same-assembly reference from FSharp.Core, or
            // a cross-assembly one to it.
            TypeRef::Named {
                assembly,
                namespace,
                name,
                type_args,
                segment_arities,
            } if is_fsharp_core(assembly.as_ref().unwrap_or(self.declaring))
                && is_fsharp_core_namespace(namespace) =>
            {
                if type_args
                    .iter()
                    .any(|a| a.nullability == Nullability::Annotated)
                {
                    return Err(FunctionTyGap::Type);
                }
                match (
                    name.as_str(),
                    segment_arities.as_slice(),
                    type_args.as_slice(),
                ) {
                    ("FSharpFunc", [2], [arg, ret]) => Ok(Ty::Fun {
                        arg: Box::new(self.ty(&arg.ty)?),
                        ret: Box::new(self.ty(&ret.ty)?),
                    }),
                    ("Unit", [0], []) => Ok(Ty::named(UNIT)),
                    _ => Err(FunctionTyGap::Type),
                }
            }
            // F#'s reference tuple is the core library's `System.Tuple`, up to
            // the seven elements that need no nested `TRest`.
            TypeRef::Named {
                assembly: Some(assembly),
                namespace,
                name,
                type_args,
                segment_arities,
            } if is_core_library(assembly)
                && namespace.len() == 1
                && namespace[0] == "System"
                && name == "Tuple"
                && (2..=7).contains(&type_args.len())
                && segment_arities.as_slice() == [type_args.len()] =>
            {
                if type_args
                    .iter()
                    .any(|a| a.nullability == Nullability::Annotated)
                {
                    return Err(FunctionTyGap::Type);
                }
                Ok(Ty::Tuple(
                    type_args
                        .iter()
                        .map(|a| self.ty(&a.ty))
                        .collect::<Result<_, _>>()?,
                ))
            }
            // A non-generic, top-level named type, by the path FCS renders
            // it under — which is the IL's only when the env holds exactly
            // one such type and F# names it so too: not renamed by a
            // `[<CompiledName>]` (FCS renders the compiled name, hover the
            // source one, and `Ty` carries one path), and not a module, an
            // abbreviation or a measure, which no IL signature names. A
            // byref-like type is not one F# lets a function value take.
            TypeRef::Named {
                namespace,
                name,
                type_args,
                segment_arities,
                ..
            } if type_args.is_empty()
                && segment_arities.as_slice() == [0]
                && !name.contains('/') =>
            {
                let handle = self
                    .env
                    .lookup_type_if_certain(namespace, name, 0)
                    .ok_or(FunctionTyGap::Type)?;
                let entity = self.env.entity(handle);
                let mut path = namespace.clone();
                path.push(name.clone());
                // The env indexes types by source name, so the entity found
                // must also carry the IL name as its compiled one: a type whose
                // source name is another's IL name is not the type referenced.
                if entity.name != *name
                    || entity.namespace != *namespace
                    || entity.source_name.is_some()
                    || entity.is_byref_like
                    || !entity.generic_parameters.is_empty()
                    || matches!(
                        entity.kind,
                        EntityKind::Module | EntityKind::Abbreviation | EntityKind::Measure
                    )
                    || self.env.entity_full_name(handle) != path.join(".")
                {
                    return Err(FunctionTyGap::Type);
                }
                Ok(Ty::named_path(path))
            }
            // A vector `T[]`. A multidimensional or bounded array declines.
            TypeRef::Array {
                element,
                rank: 1,
                sizes,
                lower_bounds,
            } if sizes.is_empty() && lower_bounds.is_empty() => {
                if element.nullability == Nullability::Annotated {
                    return Err(FunctionTyGap::Type);
                }
                Ok(Ty::Array {
                    elem: Box::new(self.ty(&element.ty)?),
                    rank: 1,
                })
            }
            TypeRef::Named { .. }
            | TypeRef::Var { .. }
            | TypeRef::Array { .. }
            | TypeRef::Ptr(_)
            | TypeRef::ByRef { .. } => Err(FunctionTyGap::Type),
        }
    }
}
