//! Differential-test helpers shared by the harnesses that compare our
//! [`AssemblyEnv`] resolutions against FCS's.
//!
//! Two of them do that — `borzoi-corpus-diff`'s whole-corpus run and the LSP
//! crate's `resolve_real_project_diff` — and both need the same thing: a way to
//! decide whether the oracle's name for an imported symbol and ours name the
//! same declaration. Neither crate can import the other (corpus-diff depends on
//! `borzoi`, not the reverse), so the comparator lives here, below both.
//!
//! Gated behind the `test-support` feature: nothing at runtime compares against
//! an oracle.

use crate::{AssemblyEnv, EntityHandle, Resolution};

/// The entity FCS says a used symbol is declared in, named **structurally**.
///
/// FCS's full name for a member is a *rendering*: the enclosing type is printed
/// through `NicePrint`, so it arrives decorated with type arguments —
/// `Holder<_>.Value`, `ImmutableArray<(int -> string)>.Empty`,
/// `ImmutableArray<Probe.A,B>.Empty` (one argument, whose type is named
/// ``A,B``). Those arguments carry commas that are not separators and `>`s that
/// do not close the list, so nothing about the enclosing type can be recovered
/// from the string. It is read from the oracle's structured output instead.
///
/// A **path of segments**, not a dotted name: a compiled name may itself contain
/// a dot (`[<CompiledName "Clr.Holder">]`), so splitting one would read a single
/// entity as two. Each segment is the entity's *compiled* name — the domain the
/// assembly projection's `Entity::name` is already in — with the
/// generic-parameter count ECMA-335 declares for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaringEntity {
    /// The namespace the outermost segment sits in; empty for the global one.
    pub namespace: Vec<String>,
    /// Compiled name and generic-parameter count per segment, outermost first.
    pub path: Vec<(String, usize)>,
    /// Whether the use is a **constructor**, which names its own type: FCS
    /// reports the type's display name for it, so `Dictionary<_,_>.Enumerator()`
    /// must not compose to `Dictionary.Enumerator.Enumerator`.
    pub is_constructor: bool,
}

/// An oracle declaration named by its declaring entity plus the used symbol's
/// own name, with no rendering in it: the path `[(Holder`1, 1)]` in namespace
/// `Probe` and the leaf `Value`, for what FCS renders `Probe.Holder<_>.Value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralName {
    /// The declaring entity, segment by segment. See [`DeclaringEntity`].
    pub declaring: DeclaringEntity,
    /// The used symbol's own display name.
    pub leaf: String,
    /// The used symbol's own generic-parameter count, when it is an entity.
    ///
    /// The declaring path names the *enclosing* entity, so a nested type's own
    /// arity is not in it: `Outer<T>.Inner<U>` and `Outer<T>.Inner<U,V>` both
    /// report the path `Outer` and the leaf `Inner`. Without this a bare
    /// nested-type use could certify the wrong one.
    pub leaf_arity: Option<usize>,
}

/// Whether two assembly full names agree, modulo the double-backtick quoting
/// FCS applies to identifiers that need it (`Operators.``not```). Only the
/// delimiter pairs are removed, never a lone backtick: a quoted identifier may
/// contain one (`lex.fsl` closes the quote on a *doubled* backtick only), so
/// ``a`b`` and ``ab`` name different symbols.
///
/// Nothing else is normalised, deliberately. FCS reports an F# *module*'s
/// `FullName` as the bare display name (`Seq`), which cannot witness which
/// symbol was bound; rather than accept a leaf-only match here, `fcs-dump`
/// qualifies such a name from the entity's own `AccessPath` before it reaches a
/// caller, so this comparison stays exact.
pub fn assembly_full_name_agrees(actual: &str, expected: &str) -> bool {
    let unquote = |s: &str| s.replace("``", "");
    unquote(actual) == unquote(expected)
}

/// The oracle's full name for a symbol with its own trailing rendering swapped
/// for the name the name environment keys it by.
///
/// FCS renders an operator's full name with its *display* leaf —
/// `Microsoft.FSharp.Core.Operators.(+)` — while every name on our side is the
/// logical one, `op_Addition`, which is also what the oracle reports as the
/// symbol's `LogicalName`. Only an operator and a constructor (`.ctor`) differ
/// between the two, and a constructor's full name is its type's, which does not
/// end in its display name, so it is returned unchanged.
pub fn logical_full_name(full_name: &str, display: &str, logical: Option<&str>) -> String {
    match logical {
        Some(logical) if logical != display => match full_name.strip_suffix(display) {
            Some(head) if head.ends_with('.') => format!("{head}{logical}"),
            Some(_) | None => full_name.to_string(),
        },
        Some(_) | None => full_name.to_string(),
    }
}

/// The oracle declaration named the way **we** name it, or `None` when our own
/// resolution does not certify it — leaving the oracle's rendered name to be
/// compared exactly as it arrived.
///
/// Our own full names carry no arity at all, so against FCS's rendering a
/// *correct* resolution scored as a divergence in both directions — 518 of the
/// 530 measured on `WoofWare.PawPrint`'s main library.
///
/// Nothing about that decoration is parsed. The enclosing entity arrives
/// structurally instead ([`DeclaringEntity`]), and is matched against the
/// enclosing chain we resolved segment by segment (`chain_position`); what
/// comes back is *our* name for the entity that certified, so the two sides'
/// spelling domains never have to be reconciled at the comparison.
///
/// A constructor names its own type, so it certifies the entity alone; every
/// other symbol certifies the entity plus its own name.
///
/// The result is an *extra* accepted name, never a substituted one: FCS names a
/// constructor use by its type (`System.ArgumentOutOfRangeException`) while its
/// declaring entity and display name compose to
/// `System.ArgumentOutOfRangeException.ArgumentOutOfRangeException`, so
/// substituting would turn agreement into a divergence.
pub fn certified_expected(
    env: &AssemblyEnv,
    res: Resolution,
    structural: &StructuralName,
) -> Option<String> {
    let chain = match res {
        Resolution::Entity(handle) => env.enclosing_chain(handle),
        Resolution::Member { parent, .. } => env.enclosing_chain(parent),
        Resolution::Local(_)
        | Resolution::Item(_)
        | Resolution::Deferred(_)
        | Resolution::Unresolved => return None,
    };
    // The path names the *enclosing* entity, so for a use that is itself an
    // entity the leaf's own arity is the only thing separating
    // `Outer<T>.Inner<U>` from `Outer<T>.Inner<U,V>`.
    if let (Resolution::Entity(handle), Some(arity)) = (res, structural.leaf_arity)
        && !structural.declaring.is_constructor
        && env.entity(handle).generic_parameters.len() != arity
    {
        return None;
    }
    let position = chain_position(env, &chain, &structural.declaring)?;
    let named = env.entity_full_name(chain[position]);
    Some(if structural.declaring.is_constructor {
        named
    } else {
        format!("{named}.{}", structural.leaf)
    })
}

/// How far along `chain` the oracle's declaring path reaches, or `None` when it
/// names something our resolution did not.
///
/// Matched in **one** domain: each segment's compiled name against the assembly
/// projection's `Entity::name`, which is the same name with ECMA-335's arity
/// mangling stripped — so the suffix is dropped here too, and the arity it
/// encoded is compared as the count the oracle reports beside it. Matching
/// either that or our *source* spelling would not be injective: with
/// `[<CompiledName "C">] type A<'T>` beside `[<CompiledName "A">] type B<'T>`,
/// the oracle's `A` for a member of `B` would also match `A`'s source name.
///
/// The arity comparison is what keeps `Holder<'T>` apart from `Holder<'T,'U>`,
/// and a companion module — never generic — out of a generic entity's place.
/// The namespace pins the path to a place rather than to a shape, and is the
/// *root sentinel* rather than the string `global`, since a namespace can be
/// called that.
///
/// **Known limit.** Two entities whose compiled names differ only by an arity
/// suffix that one of them spells explicitly — `[<CompiledName "C">] type A<'T>`
/// beside `[<CompiledName "C`1">] type B<'T>` — are indistinguishable here,
/// because the assembly projection stores `C` for both: `Entity::name` has the
/// suffix stripped and the arity moved to `generic_parameters`, so the
/// distinction is gone before this comparison sees it. Closing it means giving
/// the projection a name that remembers its mangling, not tightening this
/// function; it is filed rather than worked around.
fn chain_position(
    env: &AssemblyEnv,
    chain: &[EntityHandle],
    declaring: &DeclaringEntity,
) -> Option<usize> {
    let position = declaring.path.len().checked_sub(1)?;
    if position >= chain.len() || env.entity(*chain.first()?).namespace != declaring.namespace {
        return None;
    }
    let mut enclosing_parameters = 0usize;
    for (index, (name, arity)) in declaring.path.iter().enumerate() {
        let entity = env.entity(chain[index]);
        if entity.generic_parameters.len() != *arity {
            return None;
        }
        // The projection strips ECMA-335's arity mangling; strip it here only
        // when the suffix *is* this segment's arity delta, so that a compiled
        // name which merely looks mangled — `[<CompiledName "C`1">] type A`
        // beside `[<CompiledName "C">] type B`, both non-generic — is compared
        // whole and cannot certify against the other.
        let delta = arity.checked_sub(enclosing_parameters)?;
        enclosing_parameters = *arity;
        let spelling = match name.rsplit_once('`') {
            Some((head, suffix)) if suffix == delta.to_string() => head,
            Some(_) | None => name.as_str(),
        };
        if spelling != entity.name {
            return None;
        }
    }
    (!env.is_module(chain[position])).then_some(position)
}

/// Every function and value of the F# module `module` that inference reads a type off
/// (`function_ty::module_function_scheme`), by its F# source name, with that
/// type rendered as FCS's canonical renderer renders the generalised type of a
/// value use of it (`flexed`, with type parameters renumbered by first
/// appearance). The
/// differential that grades the reading binds each name to a value and asks
/// FCS for its type.
pub fn module_function_types(env: &AssemblyEnv, module: EntityHandle) -> Vec<(String, String)> {
    env.member_indices(module)
        .filter_map(|idx| {
            let borzoi_assembly::Member::Method(m) = env.member_at(module, idx) else {
                return None;
            };
            let scheme = crate::function_ty::module_function_scheme(env, module, m)
                .ok()?
                .ty;
            let scheme = flexed(&scheme, m.generic_parameters.len() as u32);
            let mut order = Vec::new();
            first_appearance(&scheme, &mut order);
            let renamed = renumber(&scheme, &order);
            Some((
                env.member_display_name(module, idx).to_string(),
                renamed.render(),
            ))
        })
        .collect()
}

/// Every function and value of the F# module `module` that inference reads a
/// type off (`function_ty::module_function_scheme`), by its F# source name,
/// with the scheme as read: before a use loosens it, each type parameter the
/// [`Ty::Param`](crate::Ty::Param) of its index.
pub fn module_function_schemes(
    env: &AssemblyEnv,
    module: EntityHandle,
) -> Vec<(String, crate::Ty)> {
    env.member_indices(module)
        .filter_map(|idx| {
            let borzoi_assembly::Member::Method(m) = env.member_at(module, idx) else {
                return None;
            };
            let scheme = crate::function_ty::module_function_scheme(env, module, m).ok()?;
            Some((env.member_display_name(module, idx).to_string(), scheme.ty))
        })
        .collect()
}

/// The functions and values of the F# module `module` whose reading
/// (`function_ty::module_function_scheme`) carries an `equality` or
/// `comparison` constraint on a type parameter, by F# source name. Inference
/// never generalises over such a parameter left open, since no scheme it
/// stores can say `when`.
pub fn module_function_constrained(
    env: &AssemblyEnv,
    module: EntityHandle,
) -> std::collections::HashSet<String> {
    env.member_indices(module)
        .filter_map(|idx| {
            let borzoi_assembly::Member::Method(m) = env.member_at(module, idx) else {
                return None;
            };
            let scheme = crate::function_ty::module_function_scheme(env, module, m).ok()?;
            scheme
                .requirements
                .iter()
                .any(Option::is_some)
                .then(|| env.member_display_name(module, idx).to_string())
        })
        .collect()
}

/// The functions of the F# module `module` that inference types by F#'s
/// built-in rule for a statically resolved operator or conversion
/// (`function_ty::builtin_operator`), by F# source name, each with what it is
/// read as (`Arith(Add)`, `Convert { target: … }`, …).
pub fn module_builtin_operators(env: &AssemblyEnv, module: EntityHandle) -> Vec<(String, String)> {
    env.member_indices(module)
        .filter_map(|idx| {
            let borzoi_assembly::Member::Method(m) = env.member_at(module, idx) else {
                return None;
            };
            let builtin = crate::function_ty::builtin_operator(env, module, m)?;
            Some((
                env.member_display_name(module, idx).to_string(),
                format!("{builtin:?}"),
            ))
        })
        .collect()
}

/// Whether the assembly called `assembly` names the non-generic top-level
/// type `handle`: declares it, or is a facade whose type forwarders send its
/// path there. FCS names a type by the assembly it reached it through, which
/// for a corelib type FSharp.Core's references introduce (`string`) is the
/// `netstandard` facade FSharp.Core compiles against; a differential reading
/// that name asks this before calling it another type.
pub fn assembly_names_type(env: &AssemblyEnv, assembly: &str, handle: EntityHandle) -> bool {
    env.assembly_names_type(assembly, handle)
}

/// FSharp.Core's renamed generic types a [`Ty`](crate::Ty) may carry, as
/// `(namespace, compiled name, source name, display name)`, for the test that
/// checks the table against FSharp.Core itself.
pub fn fsharp_core_renamed() -> &'static [(&'static str, &'static str, &'static str, &'static str)]
{
    crate::ty::FSHARP_CORE_RENAMED
}

/// The type of an application of the function `name` of the F# module
/// `module` to `args` arguments, as the signature reading
/// (`function_ty::module_function_scheme`) gives it: the scheme with its first
/// `args` curried domains peeled off, rendered as FCS renders a type. `None`
/// when the reading declines, the function takes fewer arguments, or the
/// result still mentions a type parameter (an application leaves it to be
/// solved, so the reading alone commits no type for it).
pub fn module_function_application_type(
    env: &AssemblyEnv,
    module: EntityHandle,
    name: &str,
    args: usize,
) -> Option<String> {
    let mut ty = env.member_indices(module).find_map(|idx| {
        let borzoi_assembly::Member::Method(m) = env.member_at(module, idx) else {
            return None;
        };
        if env.member_display_name(module, idx) != name {
            return None;
        }
        crate::function_ty::module_function_scheme(env, module, m)
            .ok()
            .map(|s| s.ty)
    })?;
    for _ in 0..args {
        let crate::Ty::Fun { ret, .. } = ty else {
            return None;
        };
        ty = *ret;
    }
    let mut params = Vec::new();
    first_appearance(&ty, &mut params);
    params.is_empty().then(|| ty.render())
}

/// The type a **value use** of a function of type `ty` has in FCS: each
/// curried domain that is not sealed, and each such element of a tupled one,
/// becomes a fresh type parameter, which F# constrains to coerce to it
/// (`MakeApplicableExprWithFlex`), so `unbox` bound as a value is `'a -> 'b`,
/// not `obj -> 'b`. `System.Object` is the one unsealed type
/// [`crate::function_ty::module_function_scheme`] carries; `next` is the first
/// unused parameter index.
fn flexed(ty: &crate::Ty, mut next: u32) -> crate::Ty {
    use crate::Ty;
    let mut flex = |t: &Ty| match t {
        Ty::Named { path, args } if args.is_empty() && path == &["System", "Object"] => {
            next += 1;
            Ty::Param(next - 1)
        }
        other => other.clone(),
    };
    let mut domains = Vec::new();
    let mut ret = ty;
    while let Ty::Fun { arg, ret: next_ret } = ret {
        domains.push(match &**arg {
            Ty::Tuple(elems) => Ty::Tuple(elems.iter().map(&mut flex).collect()),
            other => flex(other),
        });
        ret = next_ret;
    }
    domains
        .into_iter()
        .rev()
        .fold(ret.clone(), |ret, arg| Ty::Fun {
            arg: Box::new(arg),
            ret: Box::new(ret),
        })
}

/// The [`Ty::Param`](crate::Ty::Param) indices of `ty` in first-appearance
/// order, argument before return.
fn first_appearance(ty: &crate::Ty, order: &mut Vec<u32>) {
    use crate::Ty;
    match ty {
        Ty::Param(i) => {
            if !order.contains(i) {
                order.push(*i);
            }
        }
        Ty::Var(_) => {}
        Ty::Named { args, .. } => args.iter().for_each(|a| first_appearance(a, order)),
        Ty::Array { elem, .. } => first_appearance(elem, order),
        Ty::Tuple(elems) => elems.iter().for_each(|e| first_appearance(e, order)),
        Ty::Fun { arg, ret } => {
            first_appearance(arg, order);
            first_appearance(ret, order);
        }
    }
}

/// `ty` with each [`Ty::Param`](crate::Ty::Param) renamed to its position in
/// `order`.
fn renumber(ty: &crate::Ty, order: &[u32]) -> crate::Ty {
    use crate::Ty;
    match ty {
        Ty::Param(i) => Ty::Param(
            order
                .iter()
                .position(|j| j == i)
                .and_then(|p| u32::try_from(p).ok())
                .unwrap_or(*i),
        ),
        Ty::Var(v) => Ty::Var(*v),
        Ty::Named { path, args } => Ty::Named {
            path: path.clone(),
            args: args.iter().map(|a| renumber(a, order)).collect(),
        },
        Ty::Array { elem, rank } => Ty::Array {
            elem: Box::new(renumber(elem, order)),
            rank: *rank,
        },
        Ty::Tuple(elems) => Ty::Tuple(elems.iter().map(|e| renumber(e, order)).collect()),
        Ty::Fun { arg, ret } => Ty::Fun {
            arg: Box::new(renumber(arg, order)),
            ret: Box::new(renumber(ret, order)),
        },
    }
}

#[cfg(test)]
mod logical_full_name_tests {
    use super::logical_full_name;

    #[test]
    fn an_operator_leaf_takes_its_logical_name() {
        assert_eq!(
            logical_full_name(
                "Microsoft.FSharp.Core.Operators.(+)",
                "(+)",
                Some("op_Addition")
            ),
            "Microsoft.FSharp.Core.Operators.op_Addition"
        );
        assert_eq!(
            logical_full_name(
                "Microsoft.FSharp.Core.Operators.(..)",
                "(..)",
                Some("op_Range")
            ),
            "Microsoft.FSharp.Core.Operators.op_Range"
        );
    }

    #[test]
    fn a_constructor_or_plain_name_is_unchanged() {
        assert_eq!(
            logical_full_name(
                "System.Collections.Generic.List",
                "ResizeArray",
                Some(".ctor")
            ),
            "System.Collections.Generic.List"
        );
        assert_eq!(
            logical_full_name("System.String.Length", "Length", Some("Length")),
            "System.String.Length"
        );
        assert_eq!(logical_full_name("Ns.T", "T", None), "Ns.T");
    }
}
