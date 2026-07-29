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
