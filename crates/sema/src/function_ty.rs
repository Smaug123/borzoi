//! The F# type of a referenced F# module function, read off its IL signature.
//!
//! An F# module function compiles to a static method whose IL signature erases
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
//! `System.Object`), an error-level `[<CompilerMessage>]`, or a
//! `[<RequiresExplicitTypeArguments>]` written without them.
//!
//! The types it carries over are deliberately few: primitives, the function's
//! own type parameters, F# function types and `unit`. Every other named type
//! is a gap, since bridging one soundly needs the checks
//! `Gen::applied_annotation_ty` makes of an annotation head (a compiled name
//! that is not the F# source name, a constrained head, an abbreviation), and no
//! caller needs one yet. A nullable-reference annotation is a gap too: `Ty`
//! cannot carry F#'s `| null`.

use borzoi_assembly::{
    AssemblyIdentity, Augmentation, EntityKind, MethodLike, Nullability, ParamDefault, Primitive,
    TypeRef, ValFacts, ValMeasures,
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
pub(crate) fn is_fsharp_core(id: &AssemblyIdentity) -> bool {
    id.name == "FSharp.Core" && id.public_key_token == Some(FSHARP_CORE_PUBLIC_KEY_TOKEN)
}

/// Why a method's IL signature cannot be read as its F# type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FunctionTyGap {
    /// Not a plain function of an F# module: a member of a type, a
    /// constructor, a value binding, an extension or augmentation member.
    NotAFunction,
    /// The F# compiler rejects a use: `[<Obsolete(_, true)>]`, an error-level
    /// `[<CompilerMessage>]`, or `[<RequiresExplicitTypeArguments>]`.
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
    /// The parameters do not map one-to-one onto curried argument groups: a
    /// tupled group, a group count the pickle did not supply, an optional or
    /// `params` parameter.
    Groups,
    /// A parameter or the return has a type this bridge does not carry.
    Type,
}

/// The F# type of `method`, a function of the F# module `handle` in `env` — as
/// a scheme: curried over its argument groups, with each of its type parameters
/// as the [`Ty::Param`] of its index, ready for instantiation.
///
/// The curried shape is read off the IL parameters only when there is exactly
/// one per argument group. That is sound because F# pickles an empty argument
/// group only as a function's *only* one (`let f () = …`, which compiles to no
/// parameters at all and so fails the count): any other `()` group is one
/// unit-typed parameter (`SynInfo.AdjustArgsForUnitElimination` in the F#
/// compiler), and a module holds no property getter, the one other shape with
/// an empty group. So with as many parameters as groups, no group is empty and
/// none holds two. A tupled group (`||>`'s `'T1 * 'T2`) fails the count and
/// declines.
pub(crate) fn module_function_scheme(
    env: &AssemblyEnv,
    handle: EntityHandle,
    method: &MethodLike,
) -> Result<Ty, FunctionTyGap> {
    let module = env.entity(handle);
    if module.kind != EntityKind::Module
        || !method.is_static
        || method.is_constructor
        || method.module_value.is_some()
        || method.is_module_value_binding
        || method.is_extension_method
        || method.augmentation != Augmentation::No
    {
        return Err(FunctionTyGap::NotAFunction);
    }
    if method.obsolete.as_ref().is_some_and(|o| o.is_error) {
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
                    if !heads
                        .iter()
                        .all(|head| env.head_hides_no_measure(handle, head))
                    {
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
    if params.is_empty()
        || method.arg_group_count != Some(params.len())
        || params
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
        declaring: &module.assembly,
        typars: method.generic_parameters.len(),
    };
    let mut ty = match &method.signature.return_type {
        // An F# function returning `unit` compiles to a `void` method.
        TypeRef::Primitive(Primitive::Void) => Ty::named(UNIT),
        ret => bridge.ty(ret)?,
    };
    for p in params.iter().rev() {
        ty = Ty::Fun {
            arg: Box::new(bridge.ty(&p.ty)?),
            ret: Box::new(ty),
        };
    }
    Ok(ty)
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
            TypeRef::Named { .. }
            | TypeRef::Var { .. }
            | TypeRef::Array { .. }
            | TypeRef::Ptr(_)
            | TypeRef::ByRef { .. } => Err(FunctionTyGap::Type),
        }
    }
}
