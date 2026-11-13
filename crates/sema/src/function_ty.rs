//! The F# type of a referenced F# module function or value, read off its IL
//! signature.
//!
//! An F# module function compiles to a static method (a value to a static
//! property, or a generic method for a type function) whose IL signature erases
//! four things its F# type has: how the parameters group into curried
//! arguments, the F#-only constraints on its type parameters, any unit of
//! measure, and (in the return position) `unit`. The signature pickle supplies
//! the first three per method — [`MethodLike::arg_groups`], each
//! parameter's [`FSharpConstraints`], and
//! [`MethodLike::val_facts`] — and the fourth is a fixed rule. With them the IL
//! signature *is* the F# type, so [`module_function_scheme`] rebuilds that type
//! as a scheme over the method's type parameters; anything the facts cannot
//! vouch for is a [`FunctionTyGap`], and the caller declines (D5). Of the
//! constraints, only `equality` and `comparison` are read into the scheme
//! ([`ModuleFunctionScheme::requirements`]), for the caller to check: they
//! unify nothing, so the IL signature is still the use's type. FSharp.Core's
//! statically resolved arithmetic operators and conversion functions are read
//! apart ([`builtin_operator`]): their signature says nothing of their result
//! until F#'s built-in rule solves the constraint, so the caller types a use
//! by the rule.
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
//! The types it carries over are: primitives, the function's own type
//! parameters, F# function types, reference tuples (`System.Tuple` of two to
//! seven elements), vectors, `unit`, and named types the env resolves to one
//! entity — non-generic, or generic and applied to arguments it carries in
//! turn (`'T list`, `Result<int, string>`, `seq<'T>`, a user's generic
//! record). A named type is carried by its compiled path, which is how FCS's
//! canonical renderer names it, so it must be the only type that path could be
//! read back as, and F# must name it as the IL does — or, for FSharp.Core's
//! renamed generics (`FSharpList` is `list`), be FSharp.Core's own, whose F#
//! name [`Ty::render_fsharp`] shows. A generic type constraining any of its
//! type parameters is a gap, since nothing checks its arguments against the
//! constraint. A nullable-reference annotation is a gap too: `Ty` cannot carry
//! F#'s `| null`.

use borzoi_assembly::{
    AbbreviationTarget, Access, ArgGroups, AssemblyIdentity, Augmentation, EntityKind,
    EqualityComparison, FSharpConstraints, MethodLike, Nullability, ParamDefault, Primitive,
    TypeParameter, TypeRef, ValFacts, ValMeasures,
};

use crate::assembly_env::{AssemblyEnv, EntityHandle};
use crate::infer::{constrained_parameter, rejected_by_attributes};
use crate::member_ty::primitive_to_ty;
use crate::ty::{FSHARP_CORE_RENAMED, Ty, TyKind, TyPath, TyTable};

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
    /// A type parameter — the function's own, or one of a generic type its
    /// signature applies — is constrained: by IL, by an F#-only constraint, or
    /// by one the pickle was not read for.
    Constrained,
    /// The F# type mentions a unit of measure the IL erased, or names a type
    /// not proven free of one.
    Measures,
    /// The parameters do not map onto argument groups in a way that is read:
    /// groups the pickle did not supply or that do not account for the
    /// parameters, a byref, an optional or a `params` parameter.
    Groups,
    /// A parameter or the return has a type this bridge does not carry.
    Type,
}

/// The F# type of a module function or value as [`module_function_scheme`]
/// reads it: the type over [`TyKind::Param`]s, and what each parameter must
/// satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleFunctionScheme {
    /// Curried over the argument groups, each type parameter the
    /// [`TyKind::Param`] of its index.
    pub(crate) ty: Ty,
    /// Per type parameter, by index: the `equality`/`comparison` constraint
    /// it carries, if any. No other constraint is read.
    pub(crate) requirements: Vec<Option<EqualityComparison>>,
}

/// What a use of a method type parameter must satisfy, if it can be checked:
/// `Ok(None)` for none, `Ok(Some(_))` for F#'s `equality`/`comparison` and
/// nothing else, `Err(())` for any constraint nothing checks — an IL one, any
/// other F#-only one, or one the pickle was not read for.
fn checkable_requirement(p: &TypeParameter) -> Result<Option<EqualityComparison>, ()> {
    let il = p.reference_type_constraint
        || p.value_type_constraint
        || p.default_constructor_constraint
        || p.is_unmanaged
        || !p.type_constraints.is_empty();
    match p.fsharp_constraints {
        _ if il => Err(()),
        FSharpConstraints::Free => Ok(None),
        FSharpConstraints::Supports(ec) => Ok(Some(ec)),
        FSharpConstraints::Present | FSharpConstraints::Unknowable => Err(()),
    }
}

/// The F# type of `method`, a function or value of the F# module `handle` in
/// `env` — as a scheme: curried over its argument groups, with each of its
/// type parameters as the [`TyKind::Param`] of its index, ready for instantiation.
///
/// A type parameter constrained only by `equality` or `comparison` is read,
/// with the constraint ([`ModuleFunctionScheme::requirements`]): those two
/// unify nothing, so the use types as the signature says, and the caller checks
/// what each one is solved to. Any other constraint is a
/// [`FunctionTyGap::Constrained`].
///
/// The argument groups are the val's pickled ones ([`curried_domains`]): a
/// module *value* (`let x = …`, or a type function such as `List.empty`,
/// which compiles to a generic method) has none, and its type is the return
/// type; a function has one domain per group, the tuple of a group's
/// parameters where it has several (`let f (a, b) c = …` is
/// `a * b -> c -> …`), and `unit` for the sole empty group (`let f () = …`).
/// F# pickles an empty group only as a function's sole one, and any other
/// `()` group is one unit-typed parameter
/// (`SynInfo.AdjustArgsForUnitElimination` in the F# compiler).
///
/// A function whose F# result is a function returns that function type in
/// the IL, so `let f x = fun y -> …` (one group) reads as `'a -> 'b -> 'c`,
/// which is its F# type.
pub(crate) fn module_function_scheme(
    env: &AssemblyEnv,
    handle: EntityHandle,
    method: &MethodLike,
) -> Result<ModuleFunctionScheme, FunctionTyGap> {
    use_checks(env, handle, method)?;
    let requirements = method
        .generic_parameters
        .iter()
        .map(checkable_requirement)
        .collect::<Result<Vec<_>, ()>>()
        .map_err(|()| FunctionTyGap::Constrained)?;
    let ty = signature_ty(env, handle, method)?;
    Ok(ModuleFunctionScheme { ty, requirements })
}

/// One of FSharp.Core's statically resolved operators or conversion
/// functions whose solution, over the primitive types, F# builds in rather
/// than looks up (`SolveMemberConstraint`'s built-in rules in the F#
/// compiler's ConstraintSolver.fs). Its signature (`^T1 -> ^T2 -> ^T3` for
/// `+`) says nothing about its result until the constraint is solved, so a
/// use is typed by the rule ([`crate::infer`]'s `builtin_verdict`), not by
/// the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Builtin {
    /// `+ - * / %`, by its compiled name.
    Arith(Arith),
    /// `&&& ||| ^^^`.
    Bitwise,
    /// `<<< >>>`.
    Shift,
    /// Prefix `-` (`~-`).
    Negate,
    /// A conversion function (`int`, `float`, `char`, …) to `target`, a
    /// numeric type or `char`.
    Convert {
        /// The type the function's signature returns: a type of the env's
        /// table ([`AssemblyEnv::types`]), as every scheme is.
        target: Ty,
    },
}

/// Which arithmetic operator a [`Builtin::Arith`] is: F# solves each over a
/// different set of types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// The [`Builtin`] `method` of the module `handle` is, if it is one: a
/// function of FSharp.Core's own `Microsoft.FSharp.Core.Operators` (by
/// assembly identity, public-key token included) whose compiled name is on
/// the closed list below, that a use may name ([`use_checks`]), whose every
/// type parameter carries an F#-only constraint and nothing in IL — the
/// statically resolved member constraint the rule solves — and whose IL
/// signature has the shape the rule assumes. A function that matches on name
/// but not on shape is not read as one: a different FSharp.Core could declare
/// a different operator under the name.
pub(crate) fn builtin_operator(
    env: &AssemblyEnv,
    handle: EntityHandle,
    method: &MethodLike,
) -> Option<Builtin> {
    // The name first: it is the cheap test, and most functions fail it.
    let shape = match method.name.as_str() {
        "op_Addition" => Shape::Arith(Arith::Add),
        "op_Subtraction" => Shape::Arith(Arith::Sub),
        "op_Multiply" => Shape::Arith(Arith::Mul),
        "op_Division" => Shape::Arith(Arith::Div),
        "op_Modulus" => Shape::Arith(Arith::Mod),
        "op_BitwiseAnd" | "op_BitwiseOr" | "op_ExclusiveOr" => Shape::Bitwise,
        "op_LeftShift" | "op_RightShift" => Shape::Shift,
        "op_UnaryNegation" => Shape::Negate,
        "ToByte" | "ToSByte" | "ToInt16" | "ToUInt16" | "ToInt" | "ToInt32" | "ToUInt"
        | "ToUInt32" | "ToInt64" | "ToUInt64" | "ToIntPtr" | "ToUIntPtr" | "ToSingle"
        | "ToDouble" | "ToDecimal" | "ToChar" => Shape::Convert,
        _ => return None,
    };
    let module = env.entity(handle);
    if !is_fsharp_core(&module.assembly)
        || env.entity_full_name(handle) != "Microsoft.FSharp.Core.Operators"
    {
        return None;
    }
    use_checks(env, handle, method).ok()?;
    if method.generic_parameters.is_empty()
        || !method.generic_parameters.iter().all(|p| {
            p.fsharp_constraints == FSharpConstraints::Present
                && !(p.reference_type_constraint
                    || p.value_type_constraint
                    || p.default_constructor_constraint
                    || p.is_unmanaged
                    || !p.type_constraints.is_empty())
        })
    {
        return None;
    }
    let ty = signature_ty(env, handle, method).ok()?;
    let tys = env.types();
    let p = |i| tys.param(i);
    let fun = |arg: Ty, ret: Ty| tys.fun(arg, ret);
    let (builtin, expected) = match shape {
        Shape::Arith(op) => (Builtin::Arith(op), fun(p(0), fun(p(1), p(2)))),
        Shape::Bitwise => (Builtin::Bitwise, fun(p(0), fun(p(0), p(0)))),
        Shape::Shift => (
            Builtin::Shift,
            fun(p(0), fun(Ty::named(env, "System.Int32"), p(0))),
        ),
        Shape::Negate => (Builtin::Negate, fun(p(0), p(0))),
        Shape::Convert => {
            let TyKind::Fun { ret: target, .. } = *tys.kind(ty) else {
                return None;
            };
            if !crate::infer::is_conversion_target(env.names(), tys, target) {
                return None;
            }
            (Builtin::Convert { target }, fun(p(0), target))
        }
    };
    let mut mentioned = Vec::new();
    collect_params(tys, expected, &mut mentioned);
    (ty == expected && mentioned.len() == method.generic_parameters.len()).then_some(builtin)
}

/// The signature shape a [`Builtin`]'s name promises, before the signature
/// is read.
#[derive(Debug, Clone, Copy)]
enum Shape {
    Arith(Arith),
    Bitwise,
    Shift,
    Negate,
    Convert,
}

/// The distinct [`TyKind::Param`] indices `ty`, a type of `tys`, mentions.
fn collect_params(tys: &TyTable, ty: Ty, out: &mut Vec<u32>) {
    match tys.kind(ty) {
        TyKind::Param(i) => {
            if !out.contains(i) {
                out.push(*i);
            }
        }
        TyKind::Var(_) => {}
        TyKind::Named { args, .. } | TyKind::Project { args, .. } | TyKind::Tuple(args) => {
            args.iter().for_each(|&a| collect_params(tys, a, out))
        }
        TyKind::Array { elem, .. } => collect_params(tys, *elem, out),
        TyKind::Fun { arg, ret } => {
            collect_params(tys, *arg, out);
            collect_params(tys, *ret, out);
        }
    }
}

/// Whether `method` is a function or value of the F# module `handle` that a
/// use may name: not a member of a type, accessible, and not one F# rejects
/// at the use for its attributes, its pickled facts, or the module it is
/// reached through; and, since IL erases them, mentioning no unit of measure.
fn use_checks(
    env: &AssemblyEnv,
    handle: EntityHandle,
    method: &MethodLike,
) -> Result<(), FunctionTyGap> {
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
    // So is any use through a module that carries an error-level
    // `[<Obsolete>]` or `[<CompilerMessage>]`, on itself or on a module
    // enclosing it: FCS checks each entity on the way to the val. A module
    // the env cannot place has enclosing entities it cannot check.
    let chain = env
        .enclosing_chain_from_root(handle)
        .ok_or(FunctionTyGap::Unread)?;
    if chain.iter().any(|&h| rejected_by_attributes(env.entity(h))) {
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
    Ok(())
}

/// The F# type of `method`, a function or value of the F# module `handle`,
/// read off its IL signature and pickled argument groups, with each of its
/// type parameters the [`TyKind::Param`] of its index. What its type parameters
/// must satisfy is the caller's to read.
fn signature_ty(
    env: &AssemblyEnv,
    handle: EntityHandle,
    method: &MethodLike,
) -> Result<Ty, FunctionTyGap> {
    let module = env.entity(handle);
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
        type_typars: 0,
    };
    let ret = match &method.signature.return_type {
        // An F# function returning `unit` compiles to a `void` method.
        TypeRef::Primitive(Primitive::Void) => Ty::named(env, UNIT),
        ret => bridge.ty(ret)?,
    };
    let params = params
        .iter()
        .map(|p| bridge.ty(&p.ty))
        .collect::<Result<Vec<_>, _>>()?;
    let domains = curried_domains(env, method, &params)?;
    let tys = env.types();
    Ok(domains
        .into_iter()
        .rev()
        .fold(ret, |ret, arg| tys.fun(arg, ret)))
}

/// The F# type of a referenced union's case, read off its maker
/// ([`UnionCaseMaker`](borzoi_assembly::UnionCaseMaker)): its fields, in
/// order, and the union type they build, each of the union's type parameters
/// the [`TyKind::Param`] of its index. A use instantiates both together.
/// Its types are the env's ([`AssemblyEnv::types`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnionCaseScheme {
    pub(crate) fields: Vec<Ty>,
    pub(crate) union: Ty,
}

impl UnionCaseScheme {
    /// The type of the case used as a value: the union itself for a nullary
    /// case, else a function from its fields — one, or the tuple of several,
    /// as F# writes `Case (a, b)` — to the union. `tys` is the table the
    /// scheme's types are of, the env's.
    pub(crate) fn value_ty(&self, tys: &TyTable) -> Ty {
        let arg = match self.fields.as_slice() {
            [] => return self.union,
            [one] => *one,
            many => tys.tuple(many),
        };
        tys.fun(arg, self.union)
    }
}

/// The F# type of case `case` of the referenced union `union`
/// ([`UnionCaseScheme`]).
///
/// fsc compiles each case to a static maker (`NewOk(T)`, FSharp.Core's
/// `Some(T)`, a nullary case's getter `get_None()`) whose parameters are the
/// case's fields as IL types, and whose return is the union applied to its own
/// type parameters. The maker is found by the case's declaration position,
/// which the pickle and fsc's `CompilationMapping` sequence number share, and
/// read through the same bridge as a module function's signature. IL erases
/// what the pickle keeps, so the pickle must vouch for the rest:
///
/// - the case's fields were read, number the maker's parameters, and mention
///   no unit of measure — directly, or behind a named type
///   ([`AssemblyEnv::head_hides_no_measure`]);
/// - the case carries no attribute: F# checks a case's attributes at each use
///   (an `[<Obsolete>]` one can reject it), and only the pickle has them; nor
///   does the union or a module enclosing it carry a rejecting one;
/// - the maker's return reads back as the union applied to its parameters in
///   order — which the bridge declines for a union constraining any of them
///   ([`Bridge::generic_path`]), since nothing here checks a use's arguments
///   against a constraint — and the maker is the only one at that position.
///
/// Anything else is a [`FunctionTyGap`], and the caller declines.
pub(crate) fn union_case_scheme(
    env: &AssemblyEnv,
    union: EntityHandle,
    case: crate::assembly_env::UnionCaseIndex,
) -> Result<UnionCaseScheme, FunctionTyGap> {
    let entity = env.entity(union);
    let borzoi_assembly::UnionCases::Known(cases) = &entity.union_cases else {
        return Err(FunctionTyGap::Unread);
    };
    let case = cases.get(case.index()).ok_or(FunctionTyGap::Unread)?;
    let borzoi_assembly::CaseFields::Read {
        count,
        measures,
        attributed,
    } = &case.fields
    else {
        return Err(FunctionTyGap::Unread);
    };
    if *attributed {
        return Err(FunctionTyGap::Rejected);
    }
    // F# checks each entity on the way to the case too — the union and every
    // module enclosing it — so an error-level `[<Obsolete>]` or
    // `[<CompilerMessage>]` on any of them rejects the use, as for a module
    // function ([`use_checks`]).
    let chain = env
        .enclosing_chain_from_root(union)
        .ok_or(FunctionTyGap::Unread)?;
    if chain.iter().any(|&h| rejected_by_attributes(env.entity(h))) {
        return Err(FunctionTyGap::Rejected);
    }
    match measures {
        ValMeasures::Measured => return Err(FunctionTyGap::Measures),
        ValMeasures::NoDirectMeasure { heads } => {
            if !heads
                .iter()
                .all(|head| env.head_hides_no_measure(union, head))
            {
                return Err(FunctionTyGap::Measures);
            }
        }
    }
    let mut makers = entity
        .union_case_makers
        .iter()
        .filter(|m| m.declaration_index == case.declaration_index);
    let (Some(maker), None) = (makers.next(), makers.next()) else {
        return Err(FunctionTyGap::Unread);
    };
    let method = &maker.method;
    let params = &method.signature.parameters;
    if params.len() != *count || !method.generic_parameters.is_empty() {
        return Err(FunctionTyGap::Groups);
    }
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
        declaring: &entity.assembly,
        typars: 0,
        type_typars: entity.generic_parameters.len(),
    };
    let union_ty = bridge.ty(&method.signature.return_type)?;
    let tys = env.types();
    let applied_to_own_parameters = match tys.kind(union_ty) {
        TyKind::Named { args, .. } => {
            args.iter()
                .enumerate()
                .all(|(i, &a)| a == tys.param(u32::try_from(i).expect("few type parameters")))
                && args.len() == entity.generic_parameters.len()
        }
        _ => false,
    };
    if !applied_to_own_parameters {
        return Err(FunctionTyGap::Type);
    }
    let fields = params
        .iter()
        .map(|p| bridge.ty(&p.ty))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UnionCaseScheme {
        fields,
        union: union_ty,
    })
}

/// The path [`TyKind::Named`] carries for the generic type `handle` applied in
/// an annotation, under the signature bridge's rules ([`Bridge::generic_path`]):
/// its compiled path when F# names it so, or for one of FSharp.Core's own
/// renamed generics (`Result` is `FSharpResult`) the compiled path that
/// [`Ty::render_fsharp`] shows by its F# name. `None` where the bridge
/// declines.
pub(crate) fn applied_type_path(env: &AssemblyEnv, handle: EntityHandle) -> Option<TyPath> {
    let entity = env.entity(handle);
    Bridge {
        env,
        declaring: &entity.assembly,
        typars: 0,
        type_typars: 0,
    }
    .generic_path(handle)
    .ok()
}

/// The curried domains of `method`'s F# type, from its bridged IL parameter
/// types `params` split by the val's pickled argument groups
/// ([`MethodLike::arg_groups`], [`CurriedArgs::split`](borzoi_assembly::CurriedArgs::split)): a group of one
/// argument is that argument's type, a group of several the tuple of theirs,
/// and the empty group `unit`. Groups that are not read, a leading receiver
/// (an augmentation, which is not a module function), groups that do not
/// account for exactly the parameters there are, and an empty group beside
/// others (F# pickles one only as a function's sole group) are all a
/// [`FunctionTyGap::Groups`]. A value has no group, and a function at least
/// one, which the pickle's value flag must agree with.
fn curried_domains(
    env: &AssemblyEnv,
    method: &MethodLike,
    params: &[Ty],
) -> Result<Vec<Ty>, FunctionTyGap> {
    let ArgGroups::Read(curried) = &method.arg_groups else {
        return Err(FunctionTyGap::Groups);
    };
    if curried.leading_receiver
        || method.is_module_value_binding != curried.groups.is_empty()
        || method.arg_group_count != Some(curried.groups.len())
    {
        return Err(FunctionTyGap::Groups);
    }
    let split = curried.split(params).ok_or(FunctionTyGap::Groups)?;
    let sole = split.groups.len() == 1;
    split
        .groups
        .iter()
        .map(|group| match group {
            [] if sole => Ok(Ty::named(env, UNIT)),
            [] => Err(FunctionTyGap::Groups),
            [one] => Ok(*one),
            many => Ok(env.types().tuple(many.to_vec())),
        })
        .collect()
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
/// parameters a [`TypeRef::Var`] may index. It builds in the env's table
/// ([`AssemblyEnv::types`]): what it reads is a scheme, ground over
/// [`TyKind::Param`]s.
struct Bridge<'a> {
    env: &'a AssemblyEnv,
    declaring: &'a AssemblyIdentity,
    typars: usize,
    /// How many of the declaring **type's** parameters a type-level
    /// [`TypeRef::Var`] may index, each read as the [`TyKind::Param`] of its
    /// index: a union's, for its case makers ([`union_case_scheme`]). Zero
    /// for a module function, whose module has none; a signature may then
    /// index the method's parameters or the type's, never both.
    type_typars: usize,
}

impl Bridge<'_> {
    fn ty(&self, ty: &TypeRef) -> Result<Ty, FunctionTyGap> {
        match ty {
            TypeRef::Primitive(Primitive::Void) => Err(FunctionTyGap::Type),
            TypeRef::Primitive(p) => Ok(primitive_to_ty(self.env.names(), self.env.types(), *p)),
            TypeRef::Var {
                index,
                is_method: true,
            } if usize::from(*index) < self.typars => Ok(self.env.types().param(u32::from(*index))),
            TypeRef::Var {
                index,
                is_method: false,
            } if self.typars == 0 && usize::from(*index) < self.type_typars => {
                Ok(self.env.types().param(u32::from(*index)))
            }
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
                && is_fsharp_core_namespace(namespace)
                && (type_args.is_empty() || name == "FSharpFunc") =>
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
                    ("FSharpFunc", [2], [arg, ret]) => {
                        let (arg, ret) = (self.ty(&arg.ty)?, self.ty(&ret.ty)?);
                        Ok(self.env.types().fun(arg, ret))
                    }
                    ("Unit", [0], []) => Ok(Ty::named(self.env, UNIT)),
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
                let elems = type_args
                    .iter()
                    .map(|a| self.ty(&a.ty))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(self.env.types().tuple(elems))
            }
            // A generic, top-level named type applied to its arguments, by its
            // compiled path, which is how FCS renders it: see
            // [`Bridge::generic_path`].
            TypeRef::Named {
                assembly,
                namespace,
                name,
                type_args,
                segment_arities,
            } if !type_args.is_empty()
                && segment_arities.as_slice() == [type_args.len()]
                && !name.contains('/') =>
            {
                if type_args
                    .iter()
                    .any(|a| a.nullability == Nullability::Annotated)
                {
                    return Err(FunctionTyGap::Type);
                }
                let scope = assembly.as_ref().unwrap_or(self.declaring);
                let handle = self
                    .env
                    .sole_generic_type_referenced(&scope.name, namespace, name, type_args.len())
                    .ok_or(FunctionTyGap::Type)?;
                let path = self.generic_path(handle)?;
                let args = type_args
                    .iter()
                    .map(|a| self.ty(&a.ty))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(self.env.types().app(path, args))
            }
            // A non-generic, top-level named type, by the path FCS renders
            // it under — which is the IL's only when the type the reference's
            // scope resolves to is the only one the env holds at that path
            // (`Ty` keeps no assembly, so another's would be read in its
            // place) and F# names it so too: not renamed by a
            // `[<CompiledName>]` (FCS renders the compiled name, hover the
            // source one, and `Ty` carries one path), and not a module, an
            // abbreviation or a measure, which no IL signature names. A
            // byref-like type is not one F# lets a function value take.
            TypeRef::Named {
                assembly,
                namespace,
                name,
                type_args,
                segment_arities,
            } if type_args.is_empty()
                && segment_arities.as_slice() == [0]
                && !name.contains('/') =>
            {
                let scope = assembly.as_ref().unwrap_or(self.declaring);
                let handle = self
                    .env
                    .sole_type_referenced(&scope.name, namespace, name)
                    .ok_or(FunctionTyGap::Type)?;
                let entity = self.env.entity(handle);
                let path: Vec<&str> = namespace
                    .iter()
                    .map(String::as_str)
                    .chain([name.as_str()])
                    .collect();
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
                Ok(self.env.types().named_path(self.env.names(), &path))
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
                let elem = self.ty(&element.ty)?;
                Ok(self.env.types().array(elem, 1))
            }
            TypeRef::Named { .. }
            | TypeRef::Var { .. }
            | TypeRef::Array { .. }
            | TypeRef::Ptr(_)
            | TypeRef::ByRef { .. } => Err(FunctionTyGap::Type),
        }
    }

    /// The compiled path of the generic type `handle` a signature applies,
    /// which is FCS's canonical rendering of it and so what [`TyKind::Named`]
    /// carries, when an instantiation of it may be read as written:
    ///
    /// - it is a type F# applies as written: not a module, an abbreviation, a
    ///   measure or byref-like, and not one of the heads FCS renders
    ///   structurally (`System.Tuple` and `System.ValueTuple`, and
    ///   `FSharpFunc`, which the FSharp.Core arm reads);
    /// - F# names it as the IL does — or it is one of FSharp.Core's renamed
    ///   generics ([`FSHARP_CORE_RENAMED`]), which [`Ty::render_fsharp`] shows
    ///   by their F# name, and the entity is FSharp.Core's own with that
    ///   source name;
    /// - none of its type parameters is constrained ([`constrained_parameter`]):
    ///   nothing here checks an argument against a constraint, and an IL
    ///   signature can apply a type to an argument F# would reject (an
    ///   assembly built without its pickle, or a C# one).
    ///
    /// A type with a measure parameter never gets here: IL erases the
    /// parameter, and the val's measure check declines first.
    fn generic_path(&self, handle: EntityHandle) -> Result<TyPath, FunctionTyGap> {
        let entity = self.env.entity(handle);
        if entity.is_byref_like
            || matches!(
                entity.kind,
                EntityKind::Module | EntityKind::Abbreviation | EntityKind::Measure
            )
        {
            return Err(FunctionTyGap::Type);
        }
        let path: Vec<&str> = entity
            .namespace
            .iter()
            .map(String::as_str)
            .chain([entity.name.as_str()])
            .collect();
        if matches!(
            path.as_slice(),
            ["System", "Tuple"]
                | ["System", "ValueTuple"]
                | ["Microsoft", "FSharp", "Core", "FSharpFunc"]
        ) {
            return Err(FunctionTyGap::Type);
        }
        let namespace = entity.namespace.join(".");
        let source = match &entity.source_name {
            None => entity.name.as_str(),
            Some(source)
                if is_fsharp_core(&entity.assembly)
                    && FSHARP_CORE_RENAMED.iter().any(|&(ns, compiled, src, _)| {
                        ns == namespace && compiled == entity.name && src == source
                    }) =>
            {
                source.as_str()
            }
            Some(_) => return Err(FunctionTyGap::Type),
        };
        // Top-level: its full name is its namespace and its F# name.
        let mut full = entity.namespace.clone();
        full.push(source.to_owned());
        if self.env.entity_full_name(handle) != full.join(".") {
            return Err(FunctionTyGap::Type);
        }
        if entity.generic_parameters.iter().any(constrained_parameter) {
            return Err(FunctionTyGap::Constrained);
        }
        Ok(TyPath::new(self.env.names(), &path))
    }
}

/// The type an annotation applying FSharp.Core's abbreviation `marker` to
/// `args` denotes — the abbreviation's target, as FCS names it
/// (`int option` is `Microsoft.FSharp.Core.FSharpOption<System.Int32>`,
/// `int array` is `System.Int32[]`) — or `None`.
///
/// Read only for FSharp.Core's own abbreviation: the marker's assembly is
/// FSharp.Core by name *and* public-key token ([`is_fsharp_core`]), never a
/// name spelled `option`. Its target must be the array tycon, or a tycon one
/// step away ([`AssemblyEnv::abbreviation_direct_tycon`]) applied to exactly
/// the abbreviation's own parameters in order, whose path the signature
/// bridge's rules accept ([`Bridge::generic_path`]): not renamed unless it is
/// one of FSharp.Core's renamed generics, and unconstrained. A constrained
/// abbreviation parameter declines too. `args` are types of `tys`, which the
/// result is built in.
pub(crate) fn fsharp_core_abbreviation_ty(
    env: &AssemblyEnv,
    tys: &TyTable,
    marker: EntityHandle,
    args: Vec<Ty>,
) -> Option<Ty> {
    let entity = env.entity(marker);
    if entity.kind != EntityKind::Abbreviation
        || !is_fsharp_core(&entity.assembly)
        || entity.generic_parameters.len() != args.len()
        || entity.generic_parameters.iter().any(constrained_parameter)
    {
        return None;
    }
    let Some(AbbreviationTarget::Named {
        path, args: targs, ..
    }) = entity.abbreviation_target.as_ref()
    else {
        return None;
    };
    let own_parameters = targs.len() == args.len()
        && targs
            .iter()
            .enumerate()
            .all(|(i, t)| matches!(t, AbbreviationTarget::Var(v) if usize::from(*v) == i));
    if !own_parameters {
        return None;
    }
    if crate::assembly_env::is_array_tycon_path(path) {
        let shape = path.last()?.split('`').next()?;
        let rank = u32::try_from(shape.len() - 1).ok()?;
        let [elem] = <[Ty; 1]>::try_from(args).ok()?;
        return Some(tys.array(elem, rank));
    }
    let target = env.abbreviation_direct_tycon(marker)?;
    let bridge = Bridge {
        env,
        declaring: &entity.assembly,
        typars: 0,
        type_typars: 0,
    };
    let path = bridge.generic_path(target).ok()?;
    (env.entity(target).generic_parameters.len() == args.len()).then(|| tys.app(path, args))
}
