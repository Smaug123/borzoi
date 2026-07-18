//! Project F# signature-pickle data onto the ECMA-derived [`Entity`]
//! tree.
//!
//! Phase 6c1 is a *narrow* slice of this projector: it only upgrades
//! ECMA entities to [`EntityKind::Measure`] when the pickled
//! [`PickledEntity::typar_kind`] is [`TyparKind::Measure`]. The wider
//! merge (abbreviation targets, F# typar constraints, member-side
//! `mustInline`, canonical DU/record-field order) is deferred — see
//! the phase-6c plan.
//!
//! ## Why only `Measure` in 6c1
//!
//! `[<Measure>] type T` is the simplest possible enrichment with a
//! real consumer-visible payoff: fsc emits an actual ECMA TypeDef row
//! for the measure type (with
//! `[CompilationMappingAttribute(SourceConstructFlags.Measure = 4)]`
//! and `extends System.Object`), so the ECMA projector already gives
//! us a [`EntityKind::Class`] entity at the right FQN — we just need
//! to upgrade its kind. Type abbreviations (`type IntId = int`) are
//! intentionally excluded: fsc inlines them at every call site and
//! emits no TypeDef row, so there is nothing for the merge to enrich.
//! Synthesising entities purely from the pickle would be the
//! "silent-fallback" anti-pattern that D5 rejects.
//!
//! ## Detection: `typar_kind`, not [`PickledTyconRepr::Measureable`]
//!
//! FCS stores the "is this a measure?" bit on `EntityOptData.entity_kind`
//! (a `TyparKind` enum) — *not* on the repr. A measure unit declared
//! as `[<Measure>] type m` pickles as
//! [`PickledTyconRepr::FSharpObjectModel`] (a regular CLR object-model
//! type) with `typar_kind = Measure`. [`PickledTyconRepr::Measureable`]
//! is reserved for the measure-abbreviation form
//! (`[<Measure>] type T = m * kg`), which fsc inlines and does *not*
//! emit a TypeDef for — so it has no merge target anyway.
//!
//! ## Mismatch policy
//!
//! Per D6.5, divergence between the two sources is a hard error rather
//! than a silent skip. Two mismatch shapes can occur and both raise
//! [`ImportError::FsharpPickleMergeMismatch`]:
//!
//! 1. The pickle names a measure entity whose FQN does not exist in
//!    the ECMA tree.
//! 2. The pickle names a measure entity whose ECMA kind is something
//!    other than [`EntityKind::Class`] (the only shape fsc emits for
//!    measure types — anything else means the two sources disagree
//!    about what this type *is*).
//!
//! The merge tolerates pickled entities that aren't measure types
//! and whose FQN simply isn't in the ECMA tree (private/internal
//! synthetic helpers, plain modules, etc.); only the measure variant
//! has a 6c1-defined merge action, so other repr variants are
//! traversed for nested entities and otherwise ignored.

use crate::ecma335_assembly::strip_arity;
use crate::error::ImportError;
use crate::fsharp_pickle::model::{
    IsType, PickledAttribute, PickledCcu, PickledEntity, PickledExnRepr, PickledTcRef,
    PickledTyconRepr, PickledType, PickledVal, TupleKind, TyparKind,
};
use crate::model::{
    AbbreviationTarget, Access, AssemblyIdentity, Augmentation, Entity, EntityKind,
    FsharpSourceRange, Member, SkippedMember, TypeParameter,
};
use std::collections::HashMap;

/// The `ValFlags.IsExtensionMember` bit. FCS tests it as
/// `(flags &&& 0b00000000000100000000L) <> 0L` (`TypedTree.fs:192`); the
/// `val_flags` int64 is pickled un-masked (`PickledBits` keeps this bit),
/// so it survives verbatim into [`crate::fsharp_pickle::model::PickledVal::flags`].
const VAL_FLAGS_IS_EXTENSION_MEMBER: i64 = 0b1_0000_0000;

/// The `EntityFlags.IsModuleOrNamespace` bit (`TypedTree.fs` `EntityFlags`).
/// Verified empirically against real fixture pickles: **set** for every F#
/// module (plain and `…WithSuffix`) and every namespace, **clear** for every
/// type — union, record, class, struct, exception, `[<Measure>]`, an
/// abbreviation, and a signature-hidden union. So `flags & this == 0`
/// positively identifies a *type* node, distinct from a module / namespace.
const ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE: i64 = 0b1;

/// Depth-first pre-order walk of the pickled entity tree, invoking `visit`
/// once per entity with its *container* ECMA position: the namespace prefix
/// and CLR type-nesting chain that precede it, **not** including its own name.
/// `visit` receives `(entity, is_root, namespace, type_chain)` and decides for
/// itself what — if anything — to record, appending the entity's own
/// [`clr_name`] to build its full FQN. Every overlay collector is one such
/// visitor, so this traversal (and its cycle guard) is the single copy shared
/// between them.
///
/// Passing the *container* prefix (rather than a position that already folds
/// in this entity) is what lets one traversal serve all three collectors: a
/// module/type records `{namespace, type_chain + clr_name}` and a measure
/// records the same, even though a `[<Measure>] type` pickles with an
/// `IsType::Namespace` `module_type` and so is *not* a type-chain extender for
/// its own children (see the child-accumulator match below). Folding the name
/// in here would have to pick one interpretation and break the other.
///
/// ## Namespace vs. module classification
///
/// The pickle's flat dotted path doesn't say where the namespace stops and
/// CLR type-nesting begins. A type declared at namespace level (`namespace N`
/// then `type T`) becomes a top-level ECMA entity `(namespace = N, name = T)`;
/// one declared inside a module (`module N.M` then `type T`) becomes a
/// *nested* TypeDef `T` under the module entity `(namespace = N, name = M)`,
/// with `T`'s own namespace empty. The two are distinguished by each
/// ancestor's `module_type.is_type`: [`IsType::Namespace`] extends the
/// namespace prefix, [`IsType::ModuleOrType`] / [`IsType::FSharpModuleWithSuffix`]
/// extend the type chain. Because namespaces always nest outside modules, the
/// split is unambiguous.
///
/// ## Root-entity name suppression
///
/// The pickle's [`PickledCcu::root_entity`] is a synthetic CCU-level wrapper
/// whose `logical_name` equals the CCU name (the assembly name) — *not* the
/// first user-declared namespace fragment. For MiniLibFs the root is named
/// `"MiniLibFs"` and contains a *separate* child entity also named
/// `"MiniLibFs"` (the user's `namespace MiniLibFs`); for FSharp.Core the root
/// is `"FSharp.Core"` and contains `"Microsoft"` / `"System"` / `"FSharp"`
/// children. The ECMA-side prefix is built from the user-declared fragments
/// only, so the root's `logical_name` is suppressed unconditionally. The
/// `is_root` flag distinguishes the root from its identically-named child in
/// the MiniLibFs case — a name-emptiness heuristic would instead produce
/// `"MiniLibFs.MiniLibFs.m"` there.
///
/// ## Cycle guard
///
/// A valid FCS entity graph is a tree: each entity is declared inline at
/// exactly one position, so `path` — the stamps on the current root-to-here
/// descent — never repeats. A corrupt or crafted pickle can nevertheless link
/// an ancestor's stamp as one of its own descendants: an *idempotent* OSGN
/// re-link of a byte-identical body, which
/// [`OsgnTable::link`](crate::fsharp_pickle::osgn) accepts as a no-op, so the
/// conflicting-relink guard never sees it. That back-edge would send an
/// unguarded walk into unbounded recursion — and because these overlays run on
/// the caller's normal stack (not the 64 MB
/// [`PICKLE_WALK_STACK_BYTES`](crate::fsharp_pickle) thread that wraps decode),
/// that is a stack-overflow *abort*, not a catchable error. Detecting a repeat
/// on `path` turns it into a loud, recoverable
/// [`ImportError::PickleEntityCycle`]. A *path* set (not a global visited set)
/// rejects only genuine cycles, so it can never reject a shape FCS accepts.
fn walk_entity_tree(
    pickled: &PickledCcu,
    entity_stamp: u32,
    is_root: bool,
    namespace: &[String],
    type_chain: &[String],
    path: &mut Vec<u32>,
    visit: &mut impl FnMut(u32, &PickledEntity, bool, &[String], &[String]) -> Result<(), ImportError>,
) -> Result<(), ImportError> {
    if path.contains(&entity_stamp) {
        return Err(ImportError::PickleEntityCycle {
            stamp: entity_stamp,
        });
    }
    let entity = pickled.tables.tycons.get(entity_stamp as usize).ok_or(
        ImportError::OsgnIndexOutOfRange {
            kind: "tycon (entity walk)",
            index: entity_stamp,
            max: pickled.tables.tycons.len(),
        },
    )?;

    // Report this entity with its *container* position — the prefix that
    // precedes it, not including its own name. A visitor that records this
    // entity appends its own `clr_name` (see the module/type collectors) or,
    // for a measure, does so unconditionally: the recorded FQN is always
    // `{namespace, type_chain + clr_name(entity)}`, independent of this
    // entity's own `is_type` (a `[<Measure>] type` pickles with an
    // `IsType::Namespace` `module_type`, yet is a type-chain leaf).
    visit(entity_stamp, entity, is_root, namespace, type_chain)?;

    // Accumulators for *children*: a namespace fragment extends the namespace
    // prefix, a module/type extends the type chain (with its arity-stripped
    // `clr_name`), and the synthetic root contributes neither.
    let (child_namespace, child_type_chain): (Vec<String>, Vec<String>) = if is_root {
        (namespace.to_vec(), type_chain.to_vec())
    } else {
        match entity.module_type.is_type {
            IsType::Namespace => {
                let mut ns = namespace.to_vec();
                ns.push(entity.logical_name.clone());
                (ns, type_chain.to_vec())
            }
            IsType::ModuleOrType | IsType::FSharpModuleWithSuffix => {
                let mut chain = type_chain.to_vec();
                chain.push(clr_name(entity));
                (namespace.to_vec(), chain)
            }
        }
    };

    path.push(entity_stamp);
    for &child_stamp in &entity.module_type.entities {
        walk_entity_tree(
            pickled,
            child_stamp,
            false,
            &child_namespace,
            &child_type_chain,
            path,
            visit,
        )?;
    }
    path.pop();
    Ok(())
}

// ---------------------------------------------------------------------------
// Source-name overlay (Stream-2 PR1)
// ---------------------------------------------------------------------------

/// Where one entity lives in the ECMA tree, plus the two host-pickle facts the
/// entity overlay carries onto it: its F# source name and its declaration
/// range. Module *member* source names are no longer this overlay's business:
/// the pickle-driven member list ([`apply_module_member_projection`]) sets them
/// per claimed member.
///
/// The two facts have **different participation**. Source-name stamping records
/// only module/type entities and always assigns (clearing a stale name is
/// correct). Range stamping additionally records measure leaves — a standalone
/// `[<Measure>] type m` pickles with an `IsType::Namespace` body, so it is a
/// type-chain *leaf* the module/type predicate misses — and a measure leaf's
/// `entity_source_name` is `None`, so letting it flow through source-name
/// stamping could *clear* a legitimately-set name on an arity-name-colliding
/// row. [`Self::is_module_or_type`] gates the source-name half accordingly.
struct EntityOverlayTarget {
    namespace: Vec<String>,
    type_chain: Vec<String>,
    /// The entity's F# `DisplayName` when it differs from its CLR name —
    /// `Some("Suffixed")` for the `SuffixedModule` module-suffix class,
    /// `Some("Bar")` for a `[<CompiledName("Foo")>] type Bar`, else `None`.
    /// Always `None` for a measure leaf (its `IsType::Namespace` body maps to
    /// `None`), which is why measure leaves must not participate in source-name
    /// stamping.
    entity_source_name: Option<String>,
    /// `true` for a module/type entity (participates in source-name stamping),
    /// `false` for a measure leaf (range-only).
    is_module_or_type: bool,
    /// The entity's `entity_range`, resolved to a source range. `None` on a bad
    /// file index or the degenerate `"unknown"` file.
    definition_range: Option<FsharpSourceRange>,
}

/// Apply the two per-entity host-pickle facts to the ECMA tree: each entity's
/// F# `source_name` and its declaration `definition_range`.
///
/// **Source name** replaces the IL-name attribute heuristic the projector skips
/// on the authoritative path (`detect_module_suffix_source_name` /
/// `detect_compilation_source_name`). FCS renders an entity by its
/// `DisplayName`; it is recoverable from the pickle via
/// [`IsType::FSharpModuleWithSuffix`] (strip the `"Module"` suffix from
/// `logical_name`) or the `compiled_name`/`logical_name` split. The owned
/// `Entity::name` stays the IL name (what the Roslyn differential compares);
/// `source_name` is the *additional* F# name. Module *member* source names come
/// from the pickle-driven member list ([`apply_module_member_projection`]).
///
/// **Definition range** carries the pickled `entity_range` so go-to-definition
/// can navigate a method-less or sequence-point-less entity (a value-only
/// module, a measure, an enum, an interface). Two guards separate it from the
/// source-name half:
///
/// - **Measure leaves are collected for the range only.** A standalone
///   `[<Measure>] type m` pickles with an `IsType::Namespace` body — a type-chain
///   leaf the module/type predicate misses — so the range collector also records
///   the measure-leaf predicate from [`merge_measure_entities`]
///   (`typar_kind == Measure` with a backing `FSharpObjectModel` repr). Its
///   `entity_source_name` is `None`, so it must not flow through source-name
///   stamping (which would clear a legitimately-set name on an arity-name-
///   colliding row); [`EntityOverlayTarget::is_module_or_type`] gates that.
/// - **Arity-ambiguous FQNs are declined.** `find_entity_mut` matches by name
///   alone, and `type A` / `type A<'T>` both project to the ECMA name `A`
///   (backtick-arity stripped on both sides). Range stamping commits only when
///   the correspondence is unambiguous in *both* directions: at most one
///   collected target per FQN key, and — via [`find_entity_unique_mut`] — at
///   most one ECMA sibling matching the addressed name at each chain step. An
///   ambiguous FQN under-sets (D5); the twins keep their sequence-point
///   navigation. (Source-name stamping keeps its pre-existing name-only lookup,
///   where the lossiness is harmless — arity twins share their source name.)
///
/// Same per-module FQN scoping ([`find_entity_mut`]) and single-CCU restriction
/// as [`apply_module_member_projection`] — the caller gates on the host pickle
/// describing the whole image (see `enumerate_type_defs`).
pub(crate) fn apply_entity_overlay(
    entities: &mut [Entity],
    pickled: &PickledCcu,
) -> Result<(), ImportError> {
    let mut targets = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |_stamp, entity, is_root, namespace, type_chain| {
            if is_root {
                return Ok(());
            }
            // A module/type extends the type chain and participates in both
            // facts. A measure leaf (`typar_kind == Measure` + a backing
            // TypeDef) is *not* a module/type — it pickles an `IsType::Namespace`
            // body — but is still a type-chain leaf keyed by the same FQN, so it
            // contributes a range-only target. The two are disjoint (a measure
            // never has a module/type body), so an entity is at most one kind.
            let is_module_or_type = matches!(
                entity.module_type.is_type,
                IsType::ModuleOrType | IsType::FSharpModuleWithSuffix
            );
            let is_measure_leaf = entity.typar_kind == TyparKind::Measure
                && matches!(entity.repr, PickledTyconRepr::FSharpObjectModel(_));
            if is_module_or_type || is_measure_leaf {
                let mut chain = type_chain.to_vec();
                chain.push(clr_name(entity));
                targets.push(EntityOverlayTarget {
                    namespace: namespace.to_vec(),
                    type_chain: chain,
                    entity_source_name: if is_module_or_type {
                        entity_source_name(entity)
                    } else {
                        None
                    },
                    is_module_or_type,
                    definition_range: resolve_entity_range(pickled, entity),
                });
            }
            Ok(())
        },
    )?;
    // Source-name half: module/type targets only, name-only lookup, always
    // assign (clearing a stale name is correct). Unchanged from before.
    for target in &targets {
        if target.is_module_or_type
            && let Some(ecma) = find_entity_mut(entities, &target.namespace, &target.type_chain)
        {
            ecma.source_name = target.entity_source_name.clone();
        }
    }
    // Range half: decline arity-ambiguous FQNs in both directions. First the
    // collected-target side — how many targets share each full FQN key.
    let mut fqn_target_counts: HashMap<(&[String], &[String]), usize> = HashMap::new();
    for target in &targets {
        *fqn_target_counts
            .entry((target.namespace.as_slice(), target.type_chain.as_slice()))
            .or_insert(0) += 1;
    }
    for target in &targets {
        let Some(range) = &target.definition_range else {
            continue;
        };
        if fqn_target_counts[&(target.namespace.as_slice(), target.type_chain.as_slice())] != 1 {
            continue; // Two source entities collapse onto this ECMA name.
        }
        if let Some(ecma) = find_entity_unique_mut(entities, &target.namespace, &target.type_chain)
        {
            ecma.definition_range = Some(range.clone());
        }
    }
    Ok(())
}

/// The F# `DisplayName` of an entity when it differs from its CLR name (the
/// owned `Entity::name`), else `None`. Mirrors FCS's `Entity.DisplayName`.
fn entity_source_name(entity: &PickledEntity) -> Option<String> {
    match entity.module_type.is_type {
        // A module sharing its name with a type compiles to `<Name>Module`
        // (`[<CompilationRepresentation(ModuleSuffix)>]` or the automatic form);
        // the source name strips that suffix (`ListModule` ⇒ `List`). `logical_name`
        // carries the suffix, and `clr_name` (the owned name) keeps it.
        IsType::FSharpModuleWithSuffix => entity
            .logical_name
            .strip_suffix("Module")
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        // A `[<CompiledName("Foo")>] type Bar` pickles `compiled_name = Foo`
        // (the CLR/owned name) and `logical_name = Bar` (the source name); when
        // they coincide the IL name already is the source name. A *generic*
        // entity's `logical_name` carries the CLR arity suffix (`Choice`2`),
        // which the F# `DisplayName` (and the owned `Entity::name`, via
        // `strip_arity` in `project_entity`) drops — so strip it here too.
        IsType::ModuleOrType => match &entity.compiled_name {
            Some(compiled) if *compiled != entity.logical_name => {
                Some(strip_arity(&entity.logical_name).to_string())
            }
            _ => None,
        },
        IsType::Namespace => None,
    }
}

/// Populate each F# module entity's
/// [`extension_member_names`](crate::Entity::extension_member_names) — the
/// source names of its **instance extension members** — straight from the host
/// CCU's signature pickle (overload-resolution-plan stage OV-0.5).
///
/// Unlike the per-member flag (which [`apply_module_member_projection`] now
/// resolves per *claimed* IL member), this reads the bit *per pickled val* and
/// records the val's **logical name** — the name a call site writes
/// (`recv.M`). That name-only, pre-IL-matching read is exactly what makes it a
/// *no-false-negative* signal for the overload extension-*absence* gate: it
/// does not depend on an IL member being claimable at all — no arity match,
/// no compiled-name requirement — the false-negative sources OV-0 found in
/// the per-method flag (plan §6.1(b)).
/// An `IsExtensionMember` val that is *not* an instance member (a
/// `type T with static member …`) is kept out of the instance list, matching
/// FCS's `IsInstanceMember` gate on the surface flag — a value receiver's group
/// takes only instance extensions — and recorded in the parallel
/// [`static_extension_member_names`](crate::Entity::static_extension_member_names)
/// instead, because a *type-qualified static* call's group takes exactly those
/// (EX-0; probed 2026-07-12: an `open`ed `type System.String with static member
/// Compare` joins `System.String.Compare 1` as `call:extension`). Name
/// *resolution* asks a different question — no augmentation of
/// either kind is bare- or qualified-resolvable — and reads the per-member
/// [`MethodLike::is_fsharp_extension_member`](crate::MethodLike::is_fsharp_extension_member)
/// flag, which is exact per projected member rather than keyed by name (a module
/// may hold both a `let M` and an augmentation `M`; only the latter is hidden).
///
/// Scoped to the declaring module exactly as the other overlays are: the
/// pickle walk finds each module's ECMA TypeDef by FQN before attaching. A
/// module whose ECMA row was filtered out is skipped (nothing to attach to);
/// like the extension flag, this list is an annotation, not a structural claim,
/// so a missing row under-populates rather than hard-errors. The list's
/// *completeness across the assembly* is bounded by
/// [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`](crate::AssemblyProjectionSkips::fsharp_abbreviations_unknowable)
/// — a foreign-CCU or undecodable image may declare extension members in modules
/// the host pickle never describes, which the consumer treats as unknowable.
/// One module's extension-name index, located by the ECMA entity it attaches to:
/// its namespace, its nested-type chain (outermost first), and the two name lists
/// — instance extensions and static ones (EX-0).
struct ExtensionIndexTarget {
    namespace: Vec<String>,
    type_chain: Vec<String>,
    instance: Vec<String>,
    statics: Vec<String>,
}

pub(crate) fn apply_extension_member_index(
    entities: &mut [Entity],
    pickled: &PickledCcu,
) -> Result<(), ImportError> {
    let mut targets: Vec<ExtensionIndexTarget> = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |_stamp, entity, is_root, namespace, type_chain| {
            if !is_root
                && matches!(
                    entity.module_type.is_type,
                    IsType::ModuleOrType | IsType::FSharpModuleWithSuffix
                )
            {
                let instance =
                    module_extension_member_names(pickled, &entity.module_type.vals, true)?;
                let statics =
                    module_extension_member_names(pickled, &entity.module_type.vals, false)?;
                if !instance.is_empty() || !statics.is_empty() {
                    let mut chain = type_chain.to_vec();
                    chain.push(clr_name(entity));
                    targets.push(ExtensionIndexTarget {
                        namespace: namespace.to_vec(),
                        type_chain: chain,
                        instance,
                        statics,
                    });
                }
            }
            Ok(())
        },
    )?;
    for t in targets {
        if let Some(ecma) = find_entity_mut(entities, &t.namespace, &t.type_chain) {
            ecma.extension_member_names = t.instance;
            ecma.static_extension_member_names = t.statics;
        }
    }
    Ok(())
}

/// Populate each F# union entity's
/// [`union_case_names`](crate::Entity::union_case_names) — the case names in
/// declaration order — straight from the host CCU's signature pickle
/// (module-open plan, Slice B: the fold's pattern surface).
///
/// The pickle is the *only* source: the ECMA projection drops every
/// case-bearing member (the `NewCase` constructors are `[CompilerGenerated]`,
/// the nullary-case getters are properties a union projection drops, and the
/// per-case carrier nested types exist only for the class-per-case
/// representation). Both pickled union representations carry the cases —
/// [`PickledTyconRepr::Union`] and
/// [`PickledTyconRepr::UnionWithStaticFields`]; each case's
/// `ident.name` is the F# **logical** name, which is what an `open`'s bare
/// name resolution matches (a `[<CompiledName>]` on a case renames only the
/// compiled form). Only **accessible** cases are listed (`TAccess []`): a
/// private representation contributes no case to a cross-assembly consumer,
/// and the resulting `Some(vec![])` is a real observation, distinct from the
/// absent-pickle `None` (codex round 21).
///
/// A union whose representation is **hidden by a signature** (`type Teq<'a,'b>`
/// exposed opaquely in a `.fsi` while the `.fs` defines the union, or an inline
/// `[<Sealed>]` signature) pickles its SIGNATURE with `NoRepr` — no union repr
/// at all — yet the compiled class keeps `CompilationMapping(SumType)`, so the
/// ECMA projector still classifies it `EntityKind::Union`. Such an entity is not
/// reached by the repr walk above; the second loop seals it to the same
/// knowably-empty `Some(vec![])` (a signature-hidden representation exposes zero
/// accessible cases), keyed on the ECMA `Union` kind and restricted to opaque,
/// measure-free *type* nodes (never modules / namespaces / abbreviations /
/// exceptions / measure-parameterised types).
///
/// Both passes match by (namespace, container chain, name, `typars.len()`) — the
/// mangled arity. That projected key is not injective (distinct metadata rows
/// like `U` and `U`0`, or a measure-erased `U`1`, collapse onto it), so the seal
/// (second loop) commits ONLY when the key names exactly one ECMA row in the
/// container — a collision declines. The real-case pass (first loop) does not
/// seal, so a mismatch there only loses that union's cases (a pre-existing
/// completeness gap), never misattaches them.
///
/// The real-case pass (the first loop) is a host-CCU fact, so it runs even when
/// the source-name overlay is non-authoritative (foreign pickles present) — like
/// the abbreviation markers and the extension-member index. Cross-assembly
/// completeness is bounded by
/// [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`](crate::AssemblyProjectionSkips::fsharp_abbreviations_unknowable):
/// a foreign-CCU union keeps an empty list, which consumers must read as
/// *unknowable* (a union has at least one case by construction). A union
/// whose ECMA row was filtered out is skipped — an annotation, not a
/// structural claim, exactly as the extension index treats a missing row.
pub(crate) fn apply_union_case_names(
    entities: &mut [Entity],
    pickled: &PickledCcu,
) -> Result<(), ImportError> {
    // (namespace, container chain, union name, union generic arity, cases).
    // The union's own segment is matched by (name, arity): F# legally
    // arity-overloads type names (`type Foo = A` beside `type Foo<'T> = B of
    // 'T`), and both CLR paths strip to `Foo` — a name-only match hands one
    // union the other's cases (codex round 24).
    #[allow(clippy::type_complexity)]
    let mut targets: Vec<(Vec<String>, Vec<String>, String, usize, Vec<String>)> = Vec::new();
    // A union whose representation is HIDDEN BY A SIGNATURE pickles as `NoRepr`
    // (see the second loop below) — the union-repr match above never reaches it.
    // Collect every `NoRepr` entity's identity so a still-`None` ECMA `Union` at
    // that path can be sealed to a knowably-empty case list.
    let mut hidden_repr: Vec<(Vec<String>, Vec<String>, String, usize)> = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |_stamp, entity, is_root, namespace, type_chain| {
            let cases = match &entity.repr {
                PickledTyconRepr::Union(cases) => Some(cases),
                PickledTyconRepr::UnionWithStaticFields { cases, .. } => Some(cases),
                _ => None,
            };
            // The CCU wrapper (`is_root`) is never a target on either side.
            if !is_root {
                if let Some(cases) = cases {
                    targets.push((
                        namespace.to_vec(),
                        type_chain.to_vec(),
                        clr_name(entity),
                        // The MANGLED arity (`U`1` for a one-typar union), which
                        // is how `Entity::generic_parameters.len()` is keyed for a
                        // *non-measure* union; a measure-parameterised union's real
                        // cases are lost here (its `U`1` row has zero CLR generic
                        // parameters — a pre-existing completeness gap), never
                        // misattached, since no ECMA row carries its `typars.len()`.
                        entity.typars.len(),
                        cases
                            .iter()
                            // Only `TAccess []` (public) cases: a private
                            // representation (`type U = private | Hidden`) pickles
                            // each case with a restricted access path, and a
                            // cross-assembly consumer can never name it — listing
                            // it would let a hidden case wrongly shadow a
                            // same-named earlier binding FCS resolves (codex
                            // round 21). Filtering can leave the list EMPTY,
                            // which is a real observation ("knowably zero
                            // accessible cases"), distinct from the absent-pickle
                            // `None`.
                            .filter(|c| c.access.is_empty())
                            .map(|c| c.ident.name.clone())
                            .collect(),
                    ));
                } else if entity.flags & ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE == 0
                    && entity.type_abbrev.is_none()
                    && matches!(entity.exn_repr, PickledExnRepr::None)
                    && matches!(entity.repr, PickledTyconRepr::NoRepr)
                    && is_measure_free(pickled, entity)
                {
                    // An OPAQUE, MEASURE-FREE TYPE: a *type* (the
                    // `IsModuleOrNamespace` flag bit is clear — so not a module or
                    // namespace), not an abbreviation, not an exception (`exn_repr`
                    // is `None` — an `exception U = …` alias emits no TypeDef of its
                    // own), whose representation is `NoRepr`, and with no erased
                    // `[<Measure>]` typar (so its `typars.len()` IS its CLR arity —
                    // the key names its own ECMA row, not a measure-erased sibling).
                    // A signature-hidden union is exactly this shape (a hidden class
                    // / record lands here too, but its ECMA kind is not `Union`, so
                    // the second loop's filter skips it). The second loop seals only
                    // when the projected key names EXACTLY ONE ECMA row — see there.
                    hidden_repr.push((
                        namespace.to_vec(),
                        type_chain.to_vec(),
                        clr_name(entity),
                        entity.typars.len(),
                    ));
                }
            }
            Ok(())
        },
    )?;
    for (namespace, containers, name, arity, names) in targets {
        let matches_union = |e: &Entity| e.name == name && e.generic_parameters.len() == arity;
        let target = if containers.is_empty() {
            entities
                .iter_mut()
                .find(|e| e.namespace == namespace && matches_union(e))
        } else {
            find_entity_mut(entities, &namespace, &containers)
                .and_then(|c| c.nested_types.iter_mut().find(|e| matches_union(e)))
        };
        if let Some(ecma) = target {
            ecma.union_case_names = Some(names);
        }
    }
    // Signature-hidden unions. `type Teq<'a,'b>` exposed opaquely in a `.fsi`
    // while the `.fs` has `type Teq<'a,'b> = private Teq of …` (or an inline
    // `[<Sealed>]` signature) replaces the impl's union repr with `TNoRepr` in
    // the SIGNATURE data (FCS `SignatureConformance`: `TFSharpTyconRepr r,
    // TNoRepr`), so the walk above never reached it. But the compiled class
    // still carries `CompilationMapping(SumType)`, so the ECMA projector
    // classifies it `EntityKind::Union` — and it kept `union_case_names = None`,
    // which the module-open fold (`sema` `fold_tycon_tier`) reads as "unknowable
    // hidden cases" and defers every dotted head over (a bare `List.replicate`
    // where a file-local union case `List` forces the dotted-path branch). A
    // representation hidden by the signature exposes ZERO accessible cases, so
    // the honest answer is a knowably-empty `Some(vec![])`, exactly like the
    // private-case filter above.
    //
    // **Decline any projected-key collision** — the load-bearing soundness guard
    // (codex review, six rounds). The seal matches a pickle candidate to its ECMA
    // row by the projected key `(strip_arity(name), generic_parameters.len())`,
    // which is NOT injective: distinct metadata TypeDefs collapse onto one key
    // whenever fsc erases or mangles the difference — a `--staticlink` foreign
    // `U`0` beside a host `U`, a measure-erased `U`1` beside a non-generic `U`, a
    // `[<CompiledName("U`0")>]`. Enumerating those sources is a losing game (each
    // review round surfaced a new one, and the pre-existing overlays share the
    // same latent ambiguity). Instead, seal ONLY when the key names EXACTLY ONE
    // ECMA row in the candidate's container. `is_measure_free` (above) makes the
    // candidate's `typars.len()` equal its own CLR arity, so its own row IS at
    // the key; if that row is the SOLE one there, the row we seal is provably
    // that own row, never a collision. Any collapse (≥ 2 rows at the key)
    // declines — the union keeps `None` (unknowable, the safe direction). Sound
    // without trusting `authoritative`, which a `--nointerfacedata` dependency
    // defeats (its copied TypeDefs bring no signature resource, so
    // `foreign_signature_data_present` misses them).
    //
    // TWO residual holes remain, shared with the other lossy-key overlays (the
    // first loop, the measure and source-name overlays) and tracked as the
    // project-wide soundness item (#145): the uniqueness check covers the leaf
    // only, not each ambiguous CONTAINER segment; and a DROPPED own row lets a
    // surviving foreign sibling look unique. Both need pathological
    // `[<CompiledName>]` / an undecodable type — never ordinary F#. The proper
    // fix is an injective projection key (retain the exact metadata identity),
    // which #145 closes for every overlay at once.
    for (namespace, containers, name, arity) in hidden_repr {
        let matches_key = |e: &Entity| e.name == name && e.generic_parameters.len() == arity;
        let is_target = |e: &Entity| {
            matches_key(e) && e.kind == EntityKind::Union && e.union_case_names.is_none()
        };
        if containers.is_empty() {
            let count = entities
                .iter()
                .filter(|e| e.namespace == namespace && matches_key(e))
                .count();
            if count == 1
                && let Some(ecma) = entities
                    .iter_mut()
                    .find(|e| e.namespace == namespace && is_target(e))
            {
                ecma.union_case_names = Some(Vec::new());
            }
        } else if let Some(container) = find_entity_mut(entities, &namespace, &containers) {
            let count = container
                .nested_types
                .iter()
                .filter(|e| matches_key(e))
                .count();
            if count == 1
                && let Some(ecma) = container.nested_types.iter_mut().find(|e| is_target(e))
            {
                ecma.union_case_names = Some(Vec::new());
            }
        }
    }
    Ok(())
}

/// The **logical names** of a module's extension member vals — instance ones when
/// `want_instance`, static ones otherwise. The name-only, no-IL-matching read
/// behind [`apply_extension_member_index`].
///
/// Reads `IsExtensionMember` and the `IsInstance` bit straight off each pickled val's flags
/// and records [`PickledVal::logical_name`] (the F# member name a use site
/// writes). Deliberately does **no** IL-matching bookkeeping — no compiled-name
/// requirement, no arity — because every one of those is a false-negative
/// source for the absence gate. Sorted and deduplicated for a stable,
/// set-like result.
fn module_extension_member_names(
    pickled: &PickledCcu,
    val_indices: &[u32],
    want_instance: bool,
) -> Result<Vec<String>, ImportError> {
    let mut names = Vec::new();
    for &vi in val_indices {
        let v = pickled
            .tables
            .vals
            .get(vi as usize)
            .ok_or(ImportError::OsgnIndexOutOfRange {
                kind: "val (extension-member index)",
                index: vi,
                max: pickled.tables.vals.len(),
            })?;
        let is_extension = (v.flags & VAL_FLAGS_IS_EXTENSION_MEMBER) != 0;
        let is_instance = v
            .member_info
            .as_ref()
            .is_some_and(|mi| mi.flags.is_instance);
        if is_extension && is_instance == want_instance {
            names.push(v.logical_name.clone());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// The IL parameter count a module val compiles to: the sum of its
/// [`PickledValReprInfo`](crate::fsharp_pickle::model::PickledValReprInfo)
/// argument-group lengths. On a module class every binding is a static method,
/// so the extension receiver is already the first group's single element and a
/// unit argument is a zero-length group — the sum is the MethodDef arity with no
/// receiver-prepend or unit-elision special-casing. `None` when the val carries
/// no `ValReprInfo`, in which case it cannot be matched to an IL method by arity.
fn val_il_arity(v: &PickledVal) -> Option<usize> {
    v.repr_info
        .as_ref()
        .map(|r| r.arg_repr.iter().map(Vec::len).sum())
}

// ---------------------------------------------------------------------------
// Module member-val index (pickle member-projection Slice B)
// ---------------------------------------------------------------------------

/// One pickled module/type entity's member vals, in pickle order, located by
/// FQN in the ECMA tree — the unified index behind the pickle-driven module
/// member-list cutover (`docs/completed/fsharp-pickle-member-projection-plan.md`,
/// Slice B).
///
/// PR1's per-name overlay indexes *patched* an IL-built member tree; this is
/// instead the ordered member *list* itself, carrying every fact
/// `apply_module_member_projection` (Slice C) needs to build a module's
/// `MethodLike`s from the pickle and cross-reference each to its IL MethodDef
/// by `(compiled name, arity)`.
///
/// A target is recorded for every non-namespace entity in the host pickle —
/// classes/unions/records included, with empty `vals` (their member vals live
/// in `tcaug.adhoc`, not `module_type.vals`; see the plan's §2 table). The
/// consumer gates on the matched ECMA entity being a module, exactly as
/// `apply_entity_overlay` does; keeping the empty entries preserves the
/// distinction between "this module has no member vals" and "this module is
/// not described by the host pickle at all", which the cutover's
/// missing-module policy needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleMemberTarget {
    /// The entity's namespace prefix, from the pickle walk's namespace vs.
    /// type-chain split (see `walk_entity_tree`).
    pub namespace: Vec<String>,
    /// The CLR type-nesting chain; the final segment is this entity's own
    /// arity-stripped CLR name.
    pub type_chain: Vec<String>,
    /// The vals that compile to *this entity's own TypeDef*, in pickle order
    /// (FCS's declaration order): the module's `let` bindings (no
    /// `member_info`) and its `IsExtensionMember` augmentations. FCS also
    /// stores the *intrinsic* members of types declared inside a module in
    /// that module's `module_type.vals` — on real FSharp.Core,
    /// `CompilerServices.RuntimeHelpers` carries the nested `StructBox`'s
    /// `.ctor`/`get_Value`/`get_Comparer` vals next to its own functions —
    /// but those compile onto the *nested type's* TypeDef, whose member
    /// projection stays IL-driven (plan §2), so they are excluded here.
    ///
    /// Two entries sharing an [`il_name`](ModuleMemberVal::il_name) are a
    /// compiled-name collision. Arity breaks most (`sprintf`/`ksprintf`), as
    /// the claim grouping in `rebuild_module_member_list` does, but not all:
    /// FSharp.Core's
    /// `TaskBuilderExtensions.MediumPriority` holds five
    /// `TaskBuilder.MergeSources` extension vals at the *same* arity, which
    /// only signature-level matching (through
    /// [`val_index`](ModuleMemberVal::val_index)) can tell apart.
    pub vals: Vec<ModuleMemberVal>,
}

/// What module member projection needs from one pickled val: the flags bit
/// FCS gates the surface extension flag on, and the arity of `val_il_arity` —
/// the same provenance the PR1 overlays this index subsumed used, so the two
/// generations can never have disagreed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleMemberVal {
    /// The val's index into the pickle's OSGN val table
    /// ([`PickledCcu`]`.tables.vals`) — the durable handle back to the full
    /// [`PickledVal`] (its type, attributes, ranges). This is what lets a
    /// consumer disambiguate the collisions the flattened facts cannot:
    /// same-`(il_name, il_arity)` overload sets are real (FSharp.Core's
    /// `MediumPriority` module has five), and resolving *which* IL MethodDef
    /// belongs to which val there needs the val's pickled signature.
    pub val_index: u32,
    /// The F# source name (`printfn`); differs from [`il_name`](Self::il_name)
    /// exactly when the val is `[<CompiledName>]`-renamed.
    pub logical_name: String,
    /// The explicit compiled name (`PrintFormatLine`), verbatim from the
    /// pickle: `None` for an un-renamed val whose IL name is its source name.
    pub compiled_name: Option<String>,
    /// The IL parameter count this val compiles to on the module class, per
    /// `val_il_arity`; `None` when the val carries no `ValReprInfo`, in
    /// which case it cannot be matched to an IL method by arity.
    ///
    /// `Some(0)` alone cannot distinguish a *value* binding (`let answer = 42`,
    /// no IL method at all) from a unit-taking function (`let f () = …`, a
    /// zero-parameter method) — that split is
    /// [`arg_group_count`](Self::arg_group_count)'s job.
    pub il_arity: Option<usize>,
    /// The number of curried argument groups in the val's `ValReprInfo`
    /// (`None` when it carries none). This is what separates the two IL
    /// shapes a module val compiles to:
    ///
    /// - `Some(0)` — a *value* binding (`let answer = 42`): fsc emits a static
    ///   *property* named [`il_name`](Self::il_name) (its getter MethodDef is
    ///   `get_<il_name>`), or a literal *field* when
    ///   [`is_literal`](Self::is_literal). Verified on real FSharp.Core:
    ///   `ExtraTopLevelOperators.async` pickles zero groups and compiles to
    ///   the `DefaultAsyncBuilder` static property.
    /// - `Some(n ≥ 1)` — a function/member: a MethodDef named
    ///   [`il_name`](Self::il_name) with [`il_arity`](Self::il_arity)
    ///   parameters.
    ///
    /// The owned model keeps the same fact on the IL side
    /// ([`MethodLike::arg_group_count`](crate::MethodLike::arg_group_count) /
    /// [`MethodLike::module_value`](crate::MethodLike::module_value)), so the
    /// cutover can reconcile the two.
    pub arg_group_count: Option<usize>,
    /// `MemberInfo.MemberFlags.IsInstance` — `false` for a plain module `let`
    /// (no `member_info`) and for a `type T with static member …`
    /// augmentation.
    pub is_instance: bool,
    /// Whether the val carries `MemberInfo` — FCS's `vref.IsMember`. On a module's
    /// val list this holds exactly for its **augmentations** (a nested type's
    /// intrinsic members are excluded by `module_member_vals`), and it is the
    /// bit FCS's unqualified-environment filter reads (`AddValRefsToItems`'s
    /// `not vref.IsMember`), *instance or static*.
    pub is_member: bool,
    /// The raw `ValFlags.IsExtensionMember` bit. The *surface* extension flag
    /// FCS reports is gated on `IsInstanceMember`
    /// ([`is_instance`](Self::is_instance) here): a static augmentation
    /// carries this bit but is never surfaced as an extension (see
    /// `val_facts`).
    pub is_extension: bool,
    /// Whether the val's `ValReprInfo.typar_repr` has a [`TyparKind::Type`]
    /// typar — i.e. whether fsc compiles it to a *generic IL method* (which
    /// FCS reads from the pickle rather than IL; exactly why the cutover
    /// wants this index). Measure-kinded typars are erased from IL and do
    /// not count: `LanguagePrimitives.FloatWithMeasure` is generic over a
    /// measure in F# but its MethodDef has zero generic parameters.
    pub is_generic: bool,
    /// Whether the val is a `[<Literal>]` (has a pickled constant value): fsc
    /// emits a static literal *field* with no accessor MethodDef, so there is
    /// no method (nor property getter) to cross-reference at all.
    pub is_literal: bool,
    /// Whether the val's pickled `TAccess` is unrestricted (an empty
    /// path list = `taccessPublic`). A `.fsi`-gated assembly can hold a
    /// *private* IL method sharing an exported val's compiled name and
    /// arity (the exported val is in the pickle; the hidden helper is not),
    /// so the cross-reference prefers a member of the matching IL
    /// accessibility class before falling back on name/shape/arity alone.
    pub is_public: bool,
    /// Where the val is defined in source — FCS's `Val.DefinitionRange`,
    /// resolved from the pickled `(val_range, DefinitionRange)` pair
    /// (`p_ValData` / `p_ranges`): the *second* component when present (the
    /// implementation range, which for an `.fsi`-constrained assembly names
    /// the `.fs` file), else the first. `None` when the val pickles no range
    /// (no `ValReprInfo`).
    pub definition_range: Option<FsharpSourceRange>,
}

impl ModuleMemberVal {
    /// The IL name FCS emits for this val: the explicit `[<CompiledName>]`
    /// when present, else the logical name. For a function or member
    /// ([`arg_group_count`](Self::arg_group_count) ≥ 1) this is the MethodDef
    /// name; for a value binding it is the static *property* name (whose
    /// getter MethodDef is `get_<il_name>`) or, for a literal, the field
    /// name.
    pub fn il_name(&self) -> &str {
        self.compiled_name.as_deref().unwrap_or(&self.logical_name)
    }
}

/// Build the module member-val index from the host CCU's signature pickle:
/// one [`ModuleMemberTarget`] per non-namespace entity, in pickle-tree
/// pre-order.
///
/// Read-only — nothing is applied to an ECMA tree here. Same single-CCU
/// restriction as the overlays (`apply_entity_overlay` et al.): the
/// pickle describes only its own modules, so on a multi-CCU `--standalone`
/// image the index is silently partial and the caller must not treat a
/// missing module as "has no members".
pub fn collect_module_member_targets(
    pickled: &PickledCcu,
) -> Result<Vec<ModuleMemberTarget>, ImportError> {
    let mut targets = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |_stamp, entity, is_root, namespace, type_chain| {
            if !is_root
                && matches!(
                    entity.module_type.is_type,
                    IsType::ModuleOrType | IsType::FSharpModuleWithSuffix
                )
            {
                let vals = module_member_vals(pickled, &entity.module_type.vals)?;
                let mut chain = type_chain.to_vec();
                chain.push(clr_name(entity));
                targets.push(ModuleMemberTarget {
                    namespace: namespace.to_vec(),
                    type_chain: chain,
                    vals,
                });
            }
            Ok(())
        },
    )?;
    Ok(targets)
}

/// Project a module's val list to [`ModuleMemberVal`]s, preserving order.
/// Unlike PR1's retired per-name overlay collectors this keeps the
/// no-compiled-name and generic vals they skipped for IL-matching reasons —
/// precisely the members only the pickle can supply, which the member-list
/// cutover exists to include.
///
/// What it *excludes* is the vals that do not compile to the module's own
/// TypeDef: a `member_info`-carrying val without the `IsExtensionMember` bit
/// is an *intrinsic* member of a type declared in this module (FCS stores
/// those in the enclosing module's val list — the nested `StructBox`'s
/// `.ctor`/`get_Value` sit in `RuntimeHelpers`' vals on real FSharp.Core),
/// emitted onto that type's TypeDef, whose member projection stays IL-driven
/// (plan §2). An augmentation *with* the bit — instance (`Counter.Tripled`)
/// or static (`Counter.Make.Static`) — compiles to a mangled static on the
/// module class and is kept.
fn module_member_vals(
    pickled: &PickledCcu,
    val_indices: &[u32],
) -> Result<Vec<ModuleMemberVal>, ImportError> {
    let mut out = Vec::new();
    for &vi in val_indices {
        let v = pickled
            .tables
            .vals
            .get(vi as usize)
            .ok_or(ImportError::OsgnIndexOutOfRange {
                kind: "val (module-member index)",
                index: vi,
                max: pickled.tables.vals.len(),
            })?;
        let is_extension = (v.flags & VAL_FLAGS_IS_EXTENSION_MEMBER) != 0;
        if v.member_info.is_some() && !is_extension {
            continue;
        }
        out.push(ModuleMemberVal {
            val_index: vi,
            logical_name: v.logical_name.clone(),
            compiled_name: v.compiled_name.clone(),
            il_arity: val_il_arity(v),
            arg_group_count: v.repr_info.as_ref().map(|r| r.arg_repr.len()),
            is_member: v.member_info.is_some(),
            is_instance: v
                .member_info
                .as_ref()
                .is_some_and(|mi| mi.flags.is_instance),
            is_extension,
            // Only a `Type`-kinded typar makes the IL method generic;
            // measure typars are erased from IL.
            is_generic: v
                .repr_info
                .as_ref()
                .is_some_and(|r| r.typar_repr.iter().any(|t| t.kind == TyparKind::Type)),
            is_literal: v.literal_value.is_some(),
            is_public: v.access.is_empty(),
            definition_range: resolve_definition_range(pickled, v),
        });
    }
    Ok(out)
}

/// The val's [`FsharpSourceRange`] from its pickled range pair — the
/// `DefinitionRange` component (`other_range`) when present, else the primary
/// `val_range`. The file string-index was bounds-checked at decode time
/// (`read_string_index`), so the lookup cannot dangle; `.get` keeps a
/// malformed index a silent decline rather than a panic all the same.
fn resolve_definition_range(pickled: &PickledCcu, v: &PickledVal) -> Option<FsharpSourceRange> {
    let r = v.other_range.as_ref().or(v.range.as_ref())?;
    let file = pickled.header.strings.get(r.file as usize)?.clone();
    Some(FsharpSourceRange {
        file,
        start_line: r.start.line,
        start_column: r.start.column,
        end_line: r.end.line,
        end_column: r.end.column,
    })
}

/// An *entity*'s [`FsharpSourceRange`] from its pickled `entity_range` — the
/// entity analogue of [`resolve_definition_range`]. Unlike a val, an entity
/// pickles a single (non-optional) `range`; FCS's `entity_other_range` (the
/// unpickled `.fs` position) never crosses the assembly boundary, so this is
/// the full cross-assembly fidelity, matching what FCS itself navigates to.
///
/// Declines (`None`) on a malformed string-table index (kept a silent decline
/// rather than a panic, like the val path) and on the degenerate `"unknown"`
/// file of the synthetic root CCU entity — belt-and-braces against a
/// degenerate range ever becoming a bogus navigation target (D5).
fn resolve_entity_range(pickled: &PickledCcu, entity: &PickledEntity) -> Option<FsharpSourceRange> {
    let r = &entity.range;
    let file = pickled.header.strings.get(r.file as usize)?.clone();
    if file == "unknown" {
        return None;
    }
    Some(FsharpSourceRange {
        file,
        start_line: r.start.line,
        start_column: r.start.column,
        end_line: r.end.line,
        end_column: r.end.column,
    })
}

// ---------------------------------------------------------------------------
// Pickle-driven module member list (member-projection Slice C)
// ---------------------------------------------------------------------------

/// How a module val claims its projected IL member: the pickled
/// representation decides which IL artefact fsc emitted, and therefore what
/// the claim must match on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ClaimShape {
    /// A *non-generic* zero-argument-group val — a value binding, projected
    /// as the rebranded static-property method carrying
    /// [`module_value`](crate::MethodLike::module_value).
    Value,
    /// A function/member: a plain MethodDef with this many IL parameters.
    /// This includes the zero-group **generic** vals
    /// (`typeof<'T>`/`sizeof<'T>`/`Unchecked.defaultof<'T>`): a CLR property
    /// cannot be generic, so fsc emits those as generic MethodDefs with zero
    /// parameters — `Function(0)`, not [`Value`](Self::Value). (Verified on
    /// real FSharp.Core: `Operators.TypeOf`/`TypeDefOf`/`SizeOf`,
    /// `Unchecked.DefaultOf`.)
    Function(usize),
    /// No `ValReprInfo` — the val cannot be shape- or arity-matched; it
    /// claims by name alone, and only when that is unambiguous.
    Unknown,
}

fn claim_shape(v: &ModuleMemberVal) -> ClaimShape {
    match (v.arg_group_count, v.il_arity) {
        (Some(0), _) if !v.is_generic => ClaimShape::Value,
        // Zero groups + IL-generic falls through here with `il_arity`
        // `Some(0)` — a generic method, not a property.
        (Some(_), Some(n)) => ClaimShape::Function(n),
        _ => ClaimShape::Unknown,
    }
}

/// The F# facts one val contributes to the member it claims: the source name
/// (`Some(logical)` exactly when `[<CompiledName>]`-renamed), whether the member
/// is surfaced as an instance extension (`IsExtensionMember ∧ IsInstance`, FCS's
/// `IsInstanceMember` gate), and whether it is an **F#-native augmentation** at
/// all (`IsExtensionMember ∧ IsMember`, instance *or* static — what name
/// resolution must hide from every unqualified and module-qualified lookup).
fn val_facts(v: &ModuleMemberVal) -> ValFacts {
    let source = match &v.compiled_name {
        Some(c) if *c != v.logical_name => Some(v.logical_name.clone()),
        _ => None,
    };
    ValFacts {
        source_name: source,
        instance_extension: v.is_extension && v.is_instance,
        fsharp_extension: v.is_extension && v.is_member,
        definition_range: v.definition_range.clone(),
    }
}

/// The facts [`val_facts`] reads off one val.
struct ValFacts {
    source_name: Option<String>,
    instance_extension: bool,
    fsharp_extension: bool,
    definition_range: Option<FsharpSourceRange>,
}

/// Per-claim-group facts, `None` when the group's vals disagree. A group is
/// every val sharing `(il_name, claim shape)` — the resolution the IL side
/// can actually distinguish. Same-group vals with *identical* facts keep them
/// (the five same-arity `TaskBuilder.MergeSources` extension overloads all
/// stay flagged); a group whose vals disagree **under-sets** the disagreeing
/// fact rather than guessing which val owns which MethodDef — the same
/// conservative posture the retired `should_flag_extension` /
/// `resolve_member_source_name` overlays took for their same-arity residual
/// (`docs/fcs-divergences.md`).
struct GroupFacts {
    source_name: Option<Option<String>>,
    extension: Option<bool>,
    fsharp_extension: Option<bool>,
    definition_range: Option<Option<FsharpSourceRange>>,
}

/// Rebuild `ecma`'s member list from the module's pickled vals — the Slice C
/// cutover (`docs/completed/fsharp-pickle-member-projection-plan.md`): the *list* comes
/// from the pickle (exactly FCS's member set), while each member's signature
/// and location stay those of the projected IL member it claims.
///
/// Claiming walks the vals in pickle (declaration) order; each non-literal
/// val takes the first unclaimed projected method matching its
/// [`ClaimShape`]. The rebuilt list is therefore in declaration order, and:
///
/// - an IL-only artefact can never survive (nothing claims it) — witness
///   `$W` twins and any future compiler-emitted helper fall out here rather
///   than by name heuristics;
/// - a `[<Literal>]` val is *elided deliberately* (fsc emits a literal field
///   with no accessor; neither projector surfaces module literals — see the
///   MiniLibFs fixture notes) — an elision, not a skip;
/// - a non-literal val with no claimable member, and a projected member no
///   val claims, are both **recorded** on
///   [`skipped_members`](crate::Entity::skipped_members) — loud, bounded
///   uncertainty instead of a silent wrong member list.
fn rebuild_module_member_list(ecma: &mut Entity, target: &ModuleMemberTarget) {
    // Unanimity-of-facts per claim group (see [`GroupFacts`]).
    let mut groups: HashMap<(&str, ClaimShape), GroupFacts> = HashMap::new();
    for v in &target.vals {
        if v.is_literal {
            continue;
        }
        let facts = val_facts(v);
        groups
            .entry((v.il_name(), claim_shape(v)))
            .and_modify(|g| {
                if g.source_name.as_ref() != Some(&facts.source_name) {
                    g.source_name = None;
                }
                if g.extension != Some(facts.instance_extension) {
                    g.extension = None;
                }
                if g.fsharp_extension != Some(facts.fsharp_extension) {
                    g.fsharp_extension = None;
                }
                if g.definition_range.as_ref() != Some(&facts.definition_range) {
                    g.definition_range = None;
                }
            })
            .or_insert(GroupFacts {
                source_name: Some(facts.source_name),
                extension: Some(facts.instance_extension),
                fsharp_extension: Some(facts.fsharp_extension),
                definition_range: Some(facts.definition_range),
            });
    }

    let mut pool: Vec<Option<Member>> = std::mem::take(&mut ecma.members)
        .into_iter()
        .map(Some)
        .collect();
    let mut members = Vec::new();
    for v in &target.vals {
        // A `[<Literal>]` val compiles to a **static literal field**, not a method:
        // it claims that field. It used to be elided from the member list entirely —
        // but FCS brings it into scope (fsi: `open M` then bare `LitVal` compiles), so
        // eliding it left an *invisible* bare name that no consumer could even know to
        // be conservative about (found by the Slice-A review of
        // `docs/assembly-module-open-plan.md`). The differential normaliser elides it
        // instead, mirroring what fcs-dump renders.
        if v.is_literal {
            let name = v.il_name();
            // By name: a `decimal` literal's field carries `[DecimalConstantAttribute]`
            // rather than the CLI `Literal` flag, so `is_literal` is not the test (review
            // round 7). The projector keeps a module field only if it is one or the other.
            let claimed = pool
                .iter()
                .position(|slot| matches!(slot, Some(Member::Field(f)) if f.name == name));
            match claimed {
                // `Field` carries no `source_name`, so a `[<CompiledName>]`-renamed
                // literal cannot be surfaced under its F# name — record it as a skip
                // (which makes the consumer conservative) rather than surface it under
                // the wrong one.
                Some(i)
                    if v.compiled_name
                        .as_deref()
                        .is_none_or(|c| c == v.logical_name) =>
                {
                    let Some(field) = pool[i].take() else {
                        unreachable!("position only selects occupied literal Field slots");
                    };
                    members.push(field);
                }
                _ => ecma.skipped_members.push(SkippedMember {
                    name: name.to_string(),
                    reason: format!(
                        "pickled `[<Literal>]` module val `{}` has no claimable IL literal field \
                         (absent, or `[<CompiledName>]`-renamed — `Field` carries no source name)",
                        v.logical_name
                    ),
                }),
            }
            continue;
        }
        let shape = claim_shape(v);
        let name = v.il_name();
        let matches_slot = |slot: &Option<Member>| -> bool {
            let Some(Member::Method(m)) = slot else {
                return false;
            };
            m.name == name
                && match shape {
                    ClaimShape::Value => m.module_value.is_some(),
                    ClaimShape::Function(n) => {
                        m.module_value.is_none() && m.signature.parameters.len() == n
                    }
                    ClaimShape::Unknown => true,
                }
        };
        // A `.fsi`-gated assembly can hold a *hidden* helper sharing an
        // exported val's compiled name, shape, and arity (the helper is
        // absent from the signature pickle, so nothing else disambiguates).
        // The val's own pickled accessibility class breaks the tie: a public
        // val prefers a public member and a restricted val a non-public one,
        // falling back to any shape-match so an IL/pickle accessibility
        // mismatch degrades to the old first-match rather than a false skip.
        let accessibility_matches = |slot: &Option<Member>| -> bool {
            let Some(Member::Method(m)) = slot else {
                return false;
            };
            matches!(m.access, Access::Public) == v.is_public
        };
        let claimed = match shape {
            // No representation to match on: claim by name only when that
            // is unambiguous, mirroring the under-set posture below.
            ClaimShape::Unknown => {
                let mut it = pool.iter().enumerate().filter(|(_, s)| matches_slot(s));
                match (it.next(), it.next()) {
                    (Some((i, _)), None) => Some(i),
                    _ => None,
                }
            }
            _ => pool
                .iter()
                .position(|s| matches_slot(s) && accessibility_matches(s))
                .or_else(|| pool.iter().position(matches_slot)),
        };
        match claimed {
            Some(i) => {
                let Some(Member::Method(mut m)) = pool[i].take() else {
                    unreachable!("matches_slot only selects occupied Method slots");
                };
                // The val's argument-group count is the authoritative value/function
                // split (`arg_group_count` doc): zero groups ⇒ an F# value binding —
                // whether fsc emitted it as the property `module_value` already marks
                // or, for a *generic* value (`typeof<'T>`), a generic method it cannot.
                // Carry it so semantic-token classification can colour the latter a
                // value rather than a function.
                m.is_module_value_binding = matches!(v.arg_group_count, Some(0));
                let facts = &groups[&(name, shape)];
                // Unanimous rename → the val's logical name; a conflicted
                // group under-sets. The CLR `[Extension]`-attribute flag
                // `project_method` read is never *cleared* — the pickle bit
                // covers F#-native augmentations only.
                m.source_name = facts.source_name.clone().flatten();
                // The binding's pickled `DefinitionRange` — under-set (like the
                // other facts) when a same-`(name, shape)` overload group's vals
                // disagree, since which val claimed which MethodDef is then
                // unprovable and a wrong range would navigate to a sibling
                // overload. A value binding is always a singleton group (module
                // property names are unique), so values — the members whose
                // getter has no sequence point and for whom this range is the
                // only source location — always keep theirs.
                m.definition_range = facts.definition_range.clone().flatten().map(Box::new);
                if facts.extension == Some(true) {
                    m.is_extension_method = true;
                }
                // The augmentation fact (instance *or* static) name resolution
                // hides on — authoritative here, since the val itself says so. A
                // conflicted group under-sets it like the others.
                if facts.fsharp_extension == Some(true) {
                    m.augmentation = Augmentation::Certain;
                }
                members.push(Member::Method(m));
            }
            None => ecma.skipped_members.push(SkippedMember {
                name: name.to_string(),
                reason: format!(
                    "pickled module val `{}` ({shape:?}) has no matching projected IL member",
                    v.logical_name
                ),
            }),
        }
    }
    for slot in pool.into_iter().flatten() {
        let (name, access) = match &slot {
            Member::Method(m) => (m.name.clone(), m.access),
            Member::Field(f) => (f.name.clone(), f.access),
            Member::Property(p) => (p.name.clone(), p.access),
            Member::Event(e) => (e.name.clone(), e.access),
        };
        // A *non-public* leftover is expected, not uncertainty: the signature
        // pickle describes the module's surface, and fsc's lambda-lifted
        // closures / local functions (`concatArray@29`) and private helpers
        // (`gprintf`, `dictValueType`) compile to non-public module methods
        // it never records. Retain them (the pre-cutover IL-driven list kept
        // them, and downstream consumers treat skips conservatively — the
        // overload gate's `entity_tree_has_extension` would poison on the
        // noise). A *public* leftover really is a member the authoritative
        // pickle should have described — record it.
        if matches!(access, Access::Public) {
            ecma.skipped_members.push(SkippedMember {
                name,
                reason: "projected IL member has no pickled module val (pickle-driven member list)"
                    .to_string(),
            });
        } else {
            members.push(slot);
        }
    }
    ecma.members = members;
}

/// Drive each module's member list from the host CCU's signature pickle —
/// the Slice C cutover of `docs/completed/fsharp-pickle-member-projection-plan.md`.
/// [`collect_module_member_targets`] supplies the ordered vals; every module
/// entity the pickle describes has its projected members rebuilt by
/// [`rebuild_module_member_list`] (list from the pickle, signatures from the
/// claimed IL members). This subsumes the module-member halves of the
/// source-name and extension overlays — including for *generic* vals, which
/// the retired per-method extension overlay had to skip (the §7 gap:
/// `TaskBuilder.MergeSources`, `Lazy`1.Force`).
///
/// Same single-CCU restriction as the other authoritative overlays (the
/// caller gates on it): the pickle describes only its own modules, so a
/// module absent from it (impossible on a single-CCU image, short of an
/// ECMA row the projector filtered) is left on its IL-driven member list.
/// Non-module kinds keep IL projection outright — their member vals are not
/// in `module_type.vals` (plan §2).
pub(crate) fn apply_module_member_projection(
    entities: &mut [Entity],
    pickled: &PickledCcu,
) -> Result<(), ImportError> {
    for target in collect_module_member_targets(pickled)? {
        let Some(ecma) = find_entity_mut(entities, &target.namespace, &target.type_chain) else {
            continue;
        };
        if !matches!(ecma.kind, EntityKind::Module) {
            continue;
        }
        rebuild_module_member_list(ecma, &target);
    }
    Ok(())
}

/// Where a pickled measure entity lives in the ECMA tree. The pickle's
/// flat dotted path doesn't tell us where the namespace stops and the
/// CLR type-nesting begins — but the pickle entity tree does: namespace
/// fragments carry [`IsType::Namespace`] on their `module_type`, while
/// modules/types carry [`IsType::ModuleOrType`] /
/// [`IsType::FSharpModuleWithSuffix`]. Namespaces always nest outside
/// modules, so the path splits cleanly into a namespace prefix and a
/// non-empty type chain. `type_chain[0]` names the top-level ECMA
/// entity (in `namespace`); `type_chain[1..]` are successive
/// `nested_types` descents (each carrying an empty namespace of its
/// own, per the ECMA model contract).
struct MeasureTarget {
    namespace: Vec<String>,
    type_chain: Vec<String>,
}

/// Merge the F# signature pickle's measure-type information onto the
/// ECMA-derived entity tree. Mutates `entities` in place; returns
/// `Ok(())` when the merge completes (with or without measure
/// upgrades) or [`ImportError::FsharpPickleMergeMismatch`] on a
/// disagreement between the two sources.
///
/// ## Filtering: `typar_kind == Measure` is necessary but not sufficient
///
/// FSharp.Core declares many measure entities that have no ECMA TypeDef row
/// backing them — most notably the `[<MeasureAnnotatedAbbreviation>] type
/// float<[<Measure>] 'Measure> = float` family in `prim-types.fs`, where the
/// F# compiler treats the abbreviation as a measure-annotated retargeting of
/// an existing IL type and emits no new metadata. Walking purely by
/// `typar_kind` and demanding a matching ECMA row would have us reject
/// FSharp.Core itself. The repr filter narrows the set to just the form
/// `[<Measure>] type m` (no body) that fsc compiles to an `extends
/// System.Object` TypeDef with the
/// `[CompilationMappingAttribute(SourceConstructFlags.Measure)]` marker —
/// that's the only shape with a merge target. The measure leaf's own CLR name
/// is appended to the container type chain by the visitor (a `[<Measure>]
/// type` pickles with an `IsType::Namespace` `module_type`, so
/// [`walk_entity_tree`] does not fold it in — see that function's docs).
pub(crate) fn merge_measure_entities(
    entities: &mut [Entity],
    pickled: &PickledCcu,
) -> Result<(), ImportError> {
    let mut measure_targets: Vec<MeasureTarget> = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |_stamp, entity, _is_root, namespace, type_chain| {
            // Record a target for every entity that is *both*
            // `typar_kind == Measure` AND carries the standalone-form repr
            // (`FSharpObjectModel`), which confirms a backing ECMA TypeDef
            // exists — a measure abbreviation / `MeasureAnnotatedAbbreviation`
            // has `Measure` kind but no TypeDef, so it is skipped. The synthetic
            // root is `typar_kind == Type` and filtered out by the kind check.
            // The measure leaf appends its own CLR name to the container's type
            // chain, regardless of its `module_type.is_type` (a `[<Measure>]
            // type` pickles as `IsType::Namespace` yet is a type-chain leaf).
            let entity_has_ecma_typedef =
                matches!(entity.repr, PickledTyconRepr::FSharpObjectModel(_));
            if entity.typar_kind == TyparKind::Measure && entity_has_ecma_typedef {
                let mut chain = type_chain.to_vec();
                chain.push(clr_name(entity));
                measure_targets.push(MeasureTarget {
                    namespace: namespace.to_vec(),
                    type_chain: chain,
                });
            }
            Ok(())
        },
    )?;

    for target in &measure_targets {
        let ecma =
            find_entity_mut(entities, &target.namespace, &target.type_chain).ok_or_else(|| {
                ImportError::FsharpPickleMergeMismatch {
                    detail: format!(
                        "pickled measure entity {} has no matching ECMA TypeDef",
                        fqn_display(target)
                    ),
                }
            })?;
        match ecma.kind {
            EntityKind::Class => {
                ecma.kind = EntityKind::Measure;
            }
            EntityKind::Measure => {
                // Idempotent — the merge has already run. Should not
                // happen in normal flow (each `from_resolution`
                // builds a fresh entity tree) but is harmless.
            }
            other => {
                return Err(ImportError::FsharpPickleMergeMismatch {
                    detail: format!(
                        "pickled measure entity {} has ECMA kind {other:?}, expected Class",
                        fqn_display(target)
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Apply the measure overlay to the ECMA tree, given a successfully decoded
/// host CCU's F# signature pickle.
///
/// This separates two error situations that the wider phase-6 design
/// treats very differently:
///
/// 1. **Decode failed**. The phase-6 unpickler is still incomplete — it
///    hard-errors on F# pickle shapes it cannot yet decode, most notably
///    non-`Const` attribute-argument expressions
///    (`[<SomeAttr(typeof<Foo>)>]`, enum-OR flags, …). That is a gap in
///    *us*, not a defect in the assembly. The ECMA tree the caller
///    already built is independently valid, and an incomplete *F# overlay*
///    decoder must not be able to destroy the *base* ECMA projection that
///    callers who never consult measure-kind still rely on. Decode
///    failures are therefore handled by the assembly projector: it records
///    a skipped F# overlay and leaves the ECMA tree unchanged. They do not
///    reach this merge helper.
///
/// 2. **Decode succeeded but disagrees** ([`merge_measure_entities`] returns
///    [`ImportError::FsharpPickleMergeMismatch`]). Two successfully-read
///    sources contradict each other — a measure named in the pickle has
///    no ECMA TypeDef, or the wrong kind. Per D6.5 this is a hard error,
///    propagated to fail the parse.
///
/// The correctness envelope, stated plainly: *measure enrichment is
/// applied only when the caller supplies a fully decoded host signature
/// pickle; a fully decoded pickle that disagrees with the ECMA tree is
/// fatal; a pickle the in-progress unpickler cannot decode is a recorded
/// skipped-overlay state owned by the caller, yielding the un-enriched base
/// tree.*
pub(crate) fn apply_measure_overlay(
    entities: &mut [Entity],
    pickled: &PickledCcu,
) -> Result<(), ImportError> {
    merge_measure_entities(entities, pickled)
}

// ---------------------------------------------------------------------------
// Declaration-order overlay
// ---------------------------------------------------------------------------

/// Reorder the projected entity tree into the host pickle's **declaration
/// order**. fsc's metadata row order is not declaration order (empirically:
/// nested modules appear *reversed* relative to nested types), but FCS never
/// reads metadata order for an F#-authored assembly — it walks the pickle,
/// whose `module_type.entities` lists preserve source declaration order — and
/// order is semantically load-bearing where FCS applies "later wins" rules,
/// most visibly the recursive `[<AutoOpen>]` fold (a later sibling module's
/// member shadows an earlier sibling's). Differential consumers are
/// unaffected: the test normaliser sorts entity lists.
///
/// Entities the pickle does not name (compiler-generated startup classes,
/// `<PrivateImplementationDetails>`, …) keep their relative metadata order
/// after the pickled ones (stable sort with an infinite rank).
pub(crate) fn apply_declaration_order(
    entities: &mut [Entity],
    pickled: &PickledCcu,
) -> Result<(), ImportError> {
    // Pickle-order ranks: top-level `(namespace, name)` pairs, and per
    // container `(namespace, type_chain)` the ordered child names.
    let mut top_rank: HashMap<(Vec<String>, String), usize> = HashMap::new();
    let mut child_order: HashMap<(Vec<String>, Vec<String>), Vec<String>> = HashMap::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |_stamp, entity, is_root, namespace, type_chain| {
            // This entity's own position in the ECMA tree (empty for the
            // synthetic root / namespace fragments, whose module/type
            // children are TOP-LEVEL ECMA entities).
            let own_chain: Vec<String> =
                if is_root || matches!(entity.module_type.is_type, IsType::Namespace) {
                    type_chain.to_vec()
                } else {
                    let mut c = type_chain.to_vec();
                    c.push(clr_name(entity));
                    c
                };
            let own_namespace: Vec<String> = if is_root {
                namespace.to_vec()
            } else if matches!(entity.module_type.is_type, IsType::Namespace) {
                let mut ns = namespace.to_vec();
                ns.push(entity.logical_name.clone());
                ns
            } else {
                namespace.to_vec()
            };
            for &child_stamp in &entity.module_type.entities {
                let child = pickled.tables.tycons.get(child_stamp as usize).ok_or(
                    ImportError::OsgnIndexOutOfRange {
                        kind: "tycon (declaration order)",
                        index: child_stamp,
                        max: pickled.tables.tycons.len(),
                    },
                )?;
                // Namespace fragments are not ECMA entities; only module/type
                // children materialise as TypeDef rows.
                if !matches!(
                    child.module_type.is_type,
                    IsType::ModuleOrType | IsType::FSharpModuleWithSuffix
                ) && child.type_abbrev.is_none()
                {
                    continue;
                }
                let name = clr_name(child);
                if own_chain.is_empty() {
                    let rank = top_rank.len();
                    top_rank
                        .entry((own_namespace.clone(), name))
                        .or_insert(rank);
                } else {
                    child_order
                        .entry((own_namespace.clone(), own_chain.clone()))
                        .or_default()
                        .push(name);
                }
            }
            Ok(())
        },
    )?;

    // Top-level: stable-sort so pickled entities take declaration order and
    // unpickled ones keep their relative metadata order at the end.
    let unranked = top_rank.len();
    // Avoid cloning keys per comparison: compute each entity's rank once.
    let ranks: Vec<usize> = entities
        .iter()
        .map(|e| {
            top_rank
                .get(&(e.namespace.clone(), e.name.clone()))
                .copied()
                .unwrap_or(unranked)
        })
        .collect();
    let mut order: Vec<usize> = (0..entities.len()).collect();
    order.sort_by_key(|&i| ranks[i]);
    apply_permutation(entities, &order);

    // Nested: reorder each pickled container's children the same way.
    for ((namespace, chain), names) in &child_order {
        if let Some(container) = find_entity_mut(entities, namespace, chain) {
            let rank_of = |e: &Entity| {
                names
                    .iter()
                    .position(|n| n == &e.name)
                    .unwrap_or(names.len())
            };
            let ranks: Vec<usize> = container.nested_types.iter().map(rank_of).collect();
            let mut order: Vec<usize> = (0..container.nested_types.len()).collect();
            order.sort_by_key(|&i| ranks[i]);
            apply_permutation(&mut container.nested_types, &order);
        }
    }
    Ok(())
}

/// Reorder `items` so that `items[new_i] = old items[order[new_i]]`.
fn apply_permutation(items: &mut [Entity], order: &[usize]) {
    debug_assert_eq!(items.len(), order.len());
    let mut scratch: Vec<Entity> = Vec::with_capacity(items.len());
    for &i in order {
        scratch.push(items[i].clone());
    }
    for (slot, value) in items.iter_mut().zip(scratch) {
        *slot = value;
    }
}

// ---------------------------------------------------------------------------
// Abbreviation shadow markers
// ---------------------------------------------------------------------------

/// Whether a pickled attribute list carries
/// `Microsoft.FSharp.Core.AutoOpenAttribute`, resolved through the header's
/// non-local-entity table (the attribute class lives in FSharp.Core, so its
/// tcref is always non-local for a user assembly). FCS's
/// `EntityHasWellKnownAttribute` match, on the pickle's encoding.
fn has_auto_open_attribute(pickled: &PickledCcu, attribs: &[PickledAttribute]) -> bool {
    attribs.iter().any(|a| match &a.tcref {
        PickledTcRef::NonLocal(idx) => {
            pickled
                .header
                .nlerefs
                .get(*idx as usize)
                .is_some_and(|nle| {
                    let path: Vec<&str> = nle
                        .path
                        .iter()
                        .filter_map(|&i| pickled.header.strings.get(i as usize).map(String::as_str))
                        .collect();
                    path == ["Microsoft", "FSharp", "Core", "AutoOpenAttribute"]
                })
        }
        PickledTcRef::Local(_) => false,
    })
}

/// Where one pickled type/exception abbreviation lives in the ECMA tree, plus
/// what the synthesised marker entity needs to carry — collected in one pass of
/// the entity walk, then placed in a second (a marker's container may be walked
/// after the marker's own site).
struct AbbrevMarkerSite {
    namespace: Vec<String>,
    /// The *container* type-nesting chain — empty for a namespace-level
    /// abbreviation, `["Auto"]` for one declared inside `module Auto`.
    type_chain: Vec<String>,
    name: String,
    source_name: Option<String>,
    typar_names: Vec<String>,
    /// `true` for an **exception** abbreviation (`exception Alias = Original`,
    /// pickled `exn_repr` of `Abbrev`/`Asm`): the marker takes
    /// [`EntityKind::Exception`], so an open fold sees the constructor name in
    /// value and pattern scope (codex round 22). A plain type abbreviation is
    /// `false` and keeps [`EntityKind::Abbreviation`].
    is_exception: bool,
    /// The pickled `[<AutoOpen>]` attribute, carried onto the marker: FCS
    /// auto-opens an `[<AutoOpen>] type Alias = Target`'s static content, so a
    /// marker without the flag would read as a complete, static-less surface
    /// (codex round 22).
    is_auto_open: bool,
    /// The decoded target of a plain type abbreviation (`type IntId = int` ⇒
    /// `Named { path: ["Microsoft","FSharp","Core","int"], … }`), or `None` for
    /// an exception abbreviation and for any target shape the nullary decoder
    /// slice does not model. Rides onto [`Entity::abbreviation_target`].
    abbreviation_target: Option<AbbreviationTarget>,
    /// The marker's `entity_range`, resolved from the same `PickledEntity`.
    /// Rides onto [`Entity::definition_range`] — the only navigable source
    /// location for a reachable exception-abbreviation marker. `None` on a bad
    /// file index or the degenerate `"unknown"` file.
    definition_range: Option<FsharpSourceRange>,
}

/// Map every same-assembly module/type tycon **stamp** to its full logical FQN
/// segments (`Point` in MiniLibFs ⇒ `["MiniLibFs", "Point"]`), so a pickle-`Local`
/// tcref in an abbreviation target resolves to a path. Built in one pass of the
/// entity walk — a `Local` target can point at a tycon walked *after* the
/// abbreviation's own site, so the whole map must exist before any decode.
///
/// Namespaces and the synthetic root contribute container prefix but are not
/// recorded: a tcref target is a *type*, never a namespace fragment. (A
/// `[<Measure>]` type pickles with an `IsType::Namespace` body and is likewise
/// not recorded — a measure is never a plain type abbreviation's target.)
fn local_tycon_fqns(pickled: &PickledCcu) -> Result<HashMap<u32, Vec<String>>, ImportError> {
    let mut map = HashMap::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |stamp, entity, is_root, namespace, type_chain| {
            if !is_root
                && matches!(
                    entity.module_type.is_type,
                    IsType::ModuleOrType | IsType::FSharpModuleWithSuffix
                )
            {
                let mut segs = namespace.to_vec();
                segs.extend(type_chain.iter().cloned());
                segs.push(clr_name(entity));
                map.insert(stamp, segs);
            }
            Ok(())
        },
    )?;
    Ok(map)
}

/// Decode a pickled `type_abbrev` body into the owned logical
/// [`AbbreviationTarget`], for the nullary-named + typar slice.
///
/// Returns `Ok(None)` — a fail-closed **decline** — for any well-formed shape
/// this slice does not model: a *generic instantiation* (non-empty `args`), a
/// function, a tuple, a measure, a union case, and a typar that is somehow out of
/// the entity's own scope. A declined target keeps every consumer deferring, so a
/// decline can never regress a resolution. Only a genuinely *malformed* pickle (a
/// dangling table index) is a loud `Err`, per the crate's fail-loud contract.
fn decode_abbreviation_target(
    pickled: &PickledCcu,
    entity: &PickledEntity,
    ty: &PickledType,
    local_fqns: &HashMap<u32, Vec<String>>,
) -> Result<Option<AbbreviationTarget>, ImportError> {
    match ty {
        // The generic-abbreviation quantifier: `type MyList<'T> = 'T list` pickles
        // `Forall([T], body)`, whose `Var`s reference the entity's own typars, so
        // decode the body directly.
        PickledType::Forall { body, .. } => {
            decode_abbreviation_target(pickled, entity, body, local_fqns)
        }
        // The abbreviation's own generic parameter, by position into its typars
        // (`type SelfVar<'T> = 'T` ⇒ `Var(0)`). An out-of-scope typar (should not
        // occur for a well-formed alias) declines rather than fabricates.
        PickledType::Var { typar_index, .. } => Ok(entity
            .typars
            .iter()
            .position(|&t| t == *typar_index)
            .map(|pos| AbbreviationTarget::Var(pos as u16))),
        // `int`, `string`, … materialised from the `simpletys` table: always a
        // nullary non-local named head.
        PickledType::AppSimple {
            simpletyp_index, ..
        } => {
            let nle_idx = *pickled
                .header
                .simpletys
                .get(*simpletyp_index as usize)
                .ok_or(ImportError::OsgnIndexOutOfRange {
                    kind: "simplety (abbreviation target)",
                    index: *simpletyp_index,
                    max: pickled.header.simpletys.len(),
                })?;
            Ok(Some(decode_nonlocal_named(pickled, nle_idx, Vec::new())?))
        }
        // A named application. `int[]` (the array tycon) and `int list` (the list
        // tycon) both arrive here as generic apps, so there is no special array
        // handling — decode the head and recurse into the args (fail-closed if any
        // arg is a shape we cannot model).
        PickledType::App { tcref, args, .. } => {
            match decode_targets(pickled, entity, args, local_fqns)? {
                Some(decoded) => decode_named_tcref(pickled, tcref, local_fqns, decoded),
                None => Ok(None),
            }
        }
        // A function `domain -> range`.
        PickledType::Fun { domain, range, .. } => {
            let Some(domain) = decode_abbreviation_target(pickled, entity, domain, local_fqns)?
            else {
                return Ok(None);
            };
            let Some(range) = decode_abbreviation_target(pickled, entity, range, local_fqns)?
            else {
                return Ok(None);
            };
            Ok(Some(AbbreviationTarget::Fun(
                Box::new(domain),
                Box::new(range),
            )))
        }
        // A reference (`a * b`) or struct (`struct (a * b)`) tuple. An F# tuple
        // has at least two elements; the lower-level decoder accepts any array
        // length, so a crafted/corrupt pickle could carry a 0- or 1-element tuple
        // tag — decline it rather than commit a valid-looking degenerate target.
        PickledType::Tuple { kind, elems } if elems.len() >= 2 => {
            match decode_targets(pickled, entity, elems, local_fqns)? {
                Some(elems) => Ok(Some(AbbreviationTarget::Tuple {
                    struct_kind: *kind == TupleKind::Struct,
                    elems,
                })),
                None => Ok(None),
            }
        }
        // Measure and union-case targets stay unmodelled — a fail-closed decline;
        // so does a degenerate (<2-element) tuple.
        PickledType::Measure(_) | PickledType::UCase { .. } | PickledType::Tuple { .. } => Ok(None),
    }
}

/// Decode a list of pickled types fail-closed: `Ok(Some(vec))` when *every*
/// element decodes, `Ok(None)` the moment any one declines (a partial arg list
/// or tuple is never faithful, so the whole shape declines).
fn decode_targets(
    pickled: &PickledCcu,
    entity: &PickledEntity,
    tys: &[PickledType],
    local_fqns: &HashMap<u32, Vec<String>>,
) -> Result<Option<Vec<AbbreviationTarget>>, ImportError> {
    let mut out = Vec::with_capacity(tys.len());
    for ty in tys {
        match decode_abbreviation_target(pickled, entity, ty, local_fqns)? {
            Some(target) => out.push(target),
            None => return Ok(None),
        }
    }
    Ok(Some(out))
}

/// Decode a named head from its tcref, attaching already-decoded `args`: a
/// non-local ref through the header tables, or a same-CCU `Local` stamp through
/// the pre-built [`local_tycon_fqns`] map.
fn decode_named_tcref(
    pickled: &PickledCcu,
    tcref: &PickledTcRef,
    local_fqns: &HashMap<u32, Vec<String>>,
    args: Vec<AbbreviationTarget>,
) -> Result<Option<AbbreviationTarget>, ImportError> {
    match tcref {
        PickledTcRef::NonLocal(idx) => Ok(Some(decode_nonlocal_named(pickled, *idx, args)?)),
        PickledTcRef::Local(stamp) => {
            if let Some(path) = local_fqns.get(stamp) {
                Ok(Some(AbbreviationTarget::Named {
                    ccu: None,
                    path: path.clone(),
                    args,
                }))
            } else if (*stamp as usize) < pickled.tables.tycons.len() {
                // In range, but not a recorded module/type — a well-formed shape
                // this slice does not model (a namespace or measure leaf). Decline.
                Ok(None)
            } else {
                Err(ImportError::OsgnIndexOutOfRange {
                    kind: "local tycon (abbreviation target)",
                    index: *stamp,
                    max: pickled.tables.tycons.len(),
                })
            }
        }
    }
}

/// Resolve a non-local entity-ref index into an
/// [`AbbreviationTarget::Named`] head carrying the given already-decoded `args`:
/// the CCU logical name and the dotted logical path the pickle carries, both
/// through the header tables. A dangling nleref, ccu, or string index is a
/// malformed header — a loud `Err`.
///
/// The ccu is stored **verbatim** (`Some(name)`), never folded to `None` even
/// when it equals the host assembly's name. fsc pickles a reference to the
/// current CCU's *own* type as a non-local ref whose ccu is the host name (a
/// public signature is written to be read from elsewhere), but the pickle's
/// [`CcuRef`](crate::fsharp_pickle::CcuRef) carries only a *name* — no version or
/// public-key-token — so a name equal to the host cannot be *proven* to mean the
/// host: an assembly can reference a different assembly of the same simple name
/// (an extern alias). Disambiguating host-vs-same-named-reference needs the
/// loaded assembly identities, which only the sema layer has; a decode-time
/// name fold would silently misroute that reference. `ccu = None` is reserved
/// for the pickle's `Local` tcref, the one form that *proves* same-CCU
/// membership. See `docs/abbreviation-target-projection-plan.md` §3.1.
fn decode_nonlocal_named(
    pickled: &PickledCcu,
    nle_idx: u32,
    args: Vec<AbbreviationTarget>,
) -> Result<AbbreviationTarget, ImportError> {
    let nle =
        pickled
            .header
            .nlerefs
            .get(nle_idx as usize)
            .ok_or(ImportError::OsgnIndexOutOfRange {
                kind: "nleref (abbreviation target)",
                index: nle_idx,
                max: pickled.header.nlerefs.len(),
            })?;
    let ccu = pickled
        .header
        .ccu_refs
        .get(nle.ccu as usize)
        .ok_or(ImportError::OsgnIndexOutOfRange {
            kind: "ccu ref (abbreviation target)",
            index: nle.ccu,
            max: pickled.header.ccu_refs.len(),
        })?
        .name
        .clone();
    let path = nle
        .path
        .iter()
        .map(|&i| {
            pickled.header.strings.get(i as usize).cloned().ok_or(
                ImportError::OsgnIndexOutOfRange {
                    kind: "string (abbreviation target path)",
                    index: i,
                    max: pickled.header.strings.len(),
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AbbreviationTarget::Named {
        ccu: Some(ccu),
        path,
        args,
    })
}

/// Synthesise a name-only marker [`Entity`] for each **public** pickle-only
/// declaration in the host CCU's signature: a plain type abbreviation gets an
/// [`EntityKind::Abbreviation`] marker, and an **exception abbreviation**
/// (`exception Alias = Original`) an [`EntityKind::Exception`] one — FCS folds
/// the alias into an `open`'s value and pattern scope as a constructor, so a
/// tree without the marker reads as complete while missing a name (codex
/// round 22). A `[<AutoOpen>]` attribute on an abbreviation rides onto the
/// marker for the same reason. fsc emits no ECMA TypeDef for a plain
/// abbreviation (`type IntId = int`) — it lives only in the pickled signature
/// data — so without these markers an abbreviation is invisible to every
/// consumer of the projected tree, and a name resolver that trusts the tree
/// binds a *different* same-named type where FCS binds the abbreviation.
///
/// The markers are deliberately **name-only**: no target type (decoding
/// `type_abbrev` into the owned [`TypeRef`](crate::TypeRef) model is the
/// wider-merge slice this module's header defers), no members, no base type.
/// Their `generic_parameters` carry the pickled typar names so arity-keyed
/// lookups treat them faithfully. A consumer can recognise them by kind and
/// treat a hit as "this name is taken by an abbreviation whose target we
/// cannot see" — a defer signal, not a resolution target.
///
/// Exclusions, each load-bearing:
/// - **FSharp.Core** synthesises nothing. Its abbreviations for the primitive
///   aliases (`Microsoft.FSharp.Core.int64` = `System.Int64`, …) *are* the
///   alias semantics consumers hard-code (see
///   `docs/completed/r2-annotation-typing-plan.md` §2 V3); marking them "unseeable"
///   would defer every bare primitive annotation in every file. Lifting this
///   needs full abbreviation-target decoding, not markers.
/// - **Non-public** abbreviations (a non-empty `TAccess` path list) are not
///   nameable from a referencing assembly, so they cannot shadow anything.
/// - **Measure-kind** entities (`typar_kind = Measure`, e.g.
///   `[<Measure>] type T = m * kg`): a measure name is not a *type*-position
///   name, and the measure overlay owns that channel.
/// - A pickled abbreviation whose FQN **already has an ECMA row** is skipped:
///   an existing row means the name is not metadata-invisible, and the row is
///   authoritative (mirrors the tolerant non-measure policy above — only
///   *divergence* between the two sources is fatal, absence from one is not).
///   Likewise an abbreviation whose *container* module has no ECMA row is
///   skipped — nothing to hang the marker on, and no lookup can reach it.
pub(crate) fn apply_abbreviation_markers(
    entities: &mut Vec<Entity>,
    pickled: &PickledCcu,
    assembly: &AssemblyIdentity,
) -> Result<(), ImportError> {
    if assembly.name == "FSharp.Core" {
        return Ok(());
    }
    // Built before the site walk: a `Local` abbreviation target may reference a
    // tycon walked *after* the abbreviation's own site, so the whole stamp→FQN
    // map must exist before any target is decoded.
    let local_fqns = local_tycon_fqns(pickled)?;
    let mut targets: Vec<AbbrevMarkerSite> = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |_stamp, entity, is_root, namespace, type_chain| {
            if is_root || !entity.access.is_empty() {
                return Ok(());
            }
            // An EXCEPTION abbreviation (`exception Alias = Original` /
            // `= SomeILException`): no ECMA TypeDef exists, only the pickle
            // knows the name — synthesize an Exception-kinded marker (codex
            // round 22). A `Fresh` repr is a real exception with its own
            // TypeDef; `None` is not an exception at all.
            if matches!(
                entity.exn_repr,
                PickledExnRepr::Abbrev(_) | PickledExnRepr::Asm(_)
            ) {
                targets.push(AbbrevMarkerSite {
                    namespace: namespace.to_vec(),
                    type_chain: type_chain.to_vec(),
                    name: clr_name(entity),
                    source_name: None,
                    typar_names: Vec::new(),
                    is_exception: true,
                    is_auto_open: false,
                    // An exception abbreviation's target is a constructor, not a
                    // type-position name; the decoder does not model it.
                    abbreviation_target: None,
                    definition_range: resolve_entity_range(pickled, entity),
                });
                return Ok(());
            }
            let Some(abbrev_ty) = &entity.type_abbrev else {
                return Ok(());
            };
            if entity.typar_kind == TyparKind::Measure {
                return Ok(());
            }
            let typar_names = entity
                .typars
                .iter()
                .map(|&idx| {
                    pickled
                        .tables
                        .typars
                        .get(idx as usize)
                        .map(|t| t.ident.name.clone())
                        .ok_or(ImportError::OsgnIndexOutOfRange {
                            kind: "typar (abbreviation marker)",
                            index: idx,
                            max: pickled.tables.typars.len(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            // `entity_source_name` cannot serve here: a plain abbreviation
            // pickles with an `IsType::Namespace` body (like `[<Measure>]`
            // types — see `walk_entity_tree`'s docs), which that helper maps
            // to `None`. Derive the F# display name directly: a
            // `[<CompiledName("X")>] type A = …` pickles
            // `compiled_name = "X"`, `logical_name = "A"`.
            let source_name = match &entity.compiled_name {
                Some(compiled) if *compiled != entity.logical_name => {
                    Some(strip_arity(&entity.logical_name).to_string())
                }
                _ => None,
            };
            let abbreviation_target =
                decode_abbreviation_target(pickled, entity, abbrev_ty, &local_fqns)?;
            targets.push(AbbrevMarkerSite {
                namespace: namespace.to_vec(),
                type_chain: type_chain.to_vec(),
                name: clr_name(entity),
                source_name,
                typar_names,
                is_exception: false,
                is_auto_open: has_auto_open_attribute(pickled, &entity.attribs),
                abbreviation_target,
                definition_range: resolve_entity_range(pickled, entity),
            });
            Ok(())
        },
    )?;
    for target in &targets {
        // The already-has-an-ECMA-row check is keyed exactly like the source
        // lookup a consumer performs: the F# *source* name
        // (`source_name.unwrap_or(name)` — two abbreviations may share one
        // `[<CompiledName>]` yet be distinct source types, codex round 3) and
        // the generic arity (`type Foo = { … }` and `type Foo<'T> = 'T list`
        // legally coexist, codex round 2). A suffixed MODULE companion
        // (`type Companion = string` + `module Companion`, whose overlaid
        // `source_name` matches the abbreviation) never occupies the
        // TYPE-position name — F# gives the type the bare name and the
        // module its suffix — so module rows don't suppress a marker (codex
        // round 4). Anything looser drops a marker for a name FCS still
        // binds.
        let target_lookup_name = target.source_name.as_deref().unwrap_or(&target.name);
        let same_key = |e: &Entity| {
            e.kind != EntityKind::Module
                && e.source_name.as_deref().unwrap_or(&e.name) == target_lookup_name
                && e.generic_parameters.len() == target.typar_names.len()
        };
        if target.type_chain.is_empty() {
            if !entities
                .iter()
                .any(|e| e.namespace == target.namespace && same_key(e))
            {
                let marker = abbreviation_marker(assembly, target.namespace.clone(), target);
                entities.push(marker);
            }
        } else if let Some(container) =
            find_entity_mut(entities, &target.namespace, &target.type_chain)
            && !container.nested_types.iter().any(same_key)
        {
            // Nested entities carry an empty namespace of their own — the
            // path lives on the outermost type (see `find_entity_mut`).
            let marker = abbreviation_marker(assembly, Vec::new(), target);
            container.nested_types.push(marker);
        }
    }
    Ok(())
}

/// The name-only marker entity for one abbreviation — every capability field
/// empty/false, so no consumer can mistake it for a modelled type.
fn abbreviation_marker(
    assembly: &AssemblyIdentity,
    namespace: Vec<String>,
    target: &AbbrevMarkerSite,
) -> Entity {
    Entity {
        assembly: assembly.clone(),
        namespace,
        name: target.name.clone(),
        kind: if target.is_exception {
            EntityKind::Exception
        } else {
            EntityKind::Abbreviation
        },
        access: Access::Public,
        // A name-only marker, not a real type — no meaningful sealedness.
        is_sealed: false,
        generic_parameters: target
            .typar_names
            .iter()
            .map(|name| TypeParameter {
                name: name.clone(),
                variance: crate::model::Variance::Invariant,
                reference_type_constraint: false,
                value_type_constraint: false,
                default_constructor_constraint: false,
                is_unmanaged: false,
                allows_ref_struct: false,
                nullability: crate::model::Nullability::Oblivious,
                type_constraints: Vec::new(),
            })
            .collect(),
        base_type: None,
        interfaces: Vec::new(),
        members: Vec::new(),
        skipped_members: Vec::new(),
        method_def_tokens: Vec::new(),
        nested_types: Vec::new(),
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: target.is_auto_open,
        is_require_qualified_access: false,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        obsolete: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: Vec::new(),
        source_name: target.source_name.clone(),
        // A name-only abbreviation marker is not a module, so it declares no
        // extension members.
        extension_member_names: Vec::new(),
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        custom_attrs: Vec::new(),
        // The decoded RHS of the abbreviation, or `None` for an exception
        // abbreviation and for any target shape the decoder does not model.
        abbreviation_target: target.abbreviation_target.clone(),
        // The marker's own `entity_range`, resolved at collection time. The
        // load-bearing consumer is the reachable **exception**-abbreviation
        // marker (`EntityKind::Exception`): it has no ECMA row and no method
        // tokens, so this range is its only navigable source location. A
        // type-abbreviation marker carries it inertly (sema defers a hit on it).
        definition_range: target.definition_range.clone(),
    }
}

/// The CLR TypeDef name for a pickled module/type entity, **arity-stripped**
/// to match how the ECMA projector keys the owned tree. FCS's
/// `Entity.CompiledName` is `entity_compiled_name` when set, else `LogicalName`
/// (so a `[<CompiledName("Metre")>] [<Measure>] type m` is `Metre`, not `m`),
/// but the projector stores `Entity::name = strip_arity(td.name.name)` — so a
/// *generic* entity whose CLR name carries the arity suffix (`FSharpChoice`2`)
/// is keyed as `FSharpChoice`. We strip the same suffix here, otherwise
/// [`find_entity_mut`] could not locate a generic entity by FQN. The
/// `"Module"` suffix of a `[<CompilationRepresentation(ModuleSuffix)>]` module
/// lives in `logical_name` and is not an arity, so it survives the strip.
fn clr_name(entity: &PickledEntity) -> String {
    let raw = entity
        .compiled_name
        .clone()
        .unwrap_or_else(|| entity.logical_name.clone());
    strip_arity(&raw).to_string()
}

/// Whether a pickled type has **no erased `[<Measure>]` type parameter**. A
/// measure typar carries no CLR metadata, so a measure-parameterised type's
/// mangled name and its CLR generic arity disagree: `type U<[<Measure>] 'u>`
/// emits `U`1` with **zero** CLR generic parameters, which the projector strips
/// to `name = "U"`, `generic_parameters = []` — indistinguishable from a
/// non-generic `type U` and from a same-name foreign union. The hidden-union
/// seal declines such a type (it keeps `None`, unknowable — the safe direction)
/// rather than match its `typars.len()` against the wrong ECMA row. A
/// measure-FREE type's `typars.len()` **is** its CLR arity, so `(name, arity)`
/// names its own TypeDef unambiguously.
///
/// FCS `TyparFlags.Kind` (`TypedTree.fs`): a typar is a measure iff
/// `flags &&& 0b00001000100000000 == 0b00000000100000000`. An unknown typar
/// index is treated as a measure — declining (the safe direction) rather than
/// trusting a `typars.len()` match we cannot validate.
fn is_measure_free(pickled: &PickledCcu, entity: &PickledEntity) -> bool {
    const TYPAR_KIND_MASK: i64 = 0b00001000100000000;
    const TYPAR_KIND_MEASURE: i64 = 0b00000000100000000;
    entity.typars.iter().all(|&ti| {
        pickled
            .tables
            .typars
            .get(ti as usize)
            .is_some_and(|t| t.flags & TYPAR_KIND_MASK != TYPAR_KIND_MEASURE)
    })
}

/// Render an FQN for error-message formatting. Matches the
/// dot-separated convention the rest of the assembly crate uses.
fn fqn_display(target: &MeasureTarget) -> String {
    let mut segments = target.namespace.clone();
    segments.extend(target.type_chain.iter().cloned());
    segments.join(".")
}

/// Find a mutable reference to the entity at the given path: a
/// top-level entity matched by `(namespace, type_chain[0])`, then a
/// descent through `nested_types` for each remaining segment of
/// `type_chain`. Nested entities carry an empty namespace of their own
/// (the path lives on the outermost type), so the descent matches on
/// `name` only. Returns `None` if any segment is missing.
fn find_entity_mut<'a>(
    entities: &'a mut [Entity],
    namespace: &[String],
    type_chain: &[String],
) -> Option<&'a mut Entity> {
    let (head, rest) = type_chain.split_first()?;
    let mut current = entities
        .iter_mut()
        .find(|e| e.namespace.as_slice() == namespace && e.name == *head)?;
    for segment in rest {
        current = current
            .nested_types
            .iter_mut()
            .find(|e| e.name == *segment)?;
    }
    Some(current)
}

/// Like [`find_entity_mut`], but returns `None` unless the addressed name is
/// **unambiguous at every chain step** — exactly one entity matches at the top
/// level and exactly one nested type matches each subsequent segment. This is
/// the ECMA-side half of the range overlay's arity-decline: `type A` and
/// `type A<'T>` both strip to the metadata name `A`, so two ECMA rows can carry
/// the name a single collected target addresses; stamping either would navigate
/// to the wrong twin, so we decline (D5). `find_entity_mut`, which takes the
/// first match, stays for source-name stamping, where the name-only lossiness
/// is harmless (twins share their source name).
fn find_entity_unique_mut<'a>(
    entities: &'a mut [Entity],
    namespace: &[String],
    type_chain: &[String],
) -> Option<&'a mut Entity> {
    let (head, rest) = type_chain.split_first()?;
    if entities
        .iter()
        .filter(|e| e.namespace.as_slice() == namespace && e.name == *head)
        .count()
        != 1
    {
        return None;
    }
    let mut current = entities
        .iter_mut()
        .find(|e| e.namespace.as_slice() == namespace && e.name == *head)?;
    for segment in rest {
        if current
            .nested_types
            .iter()
            .filter(|e| e.name == *segment)
            .count()
            != 1
        {
            return None;
        }
        current = current
            .nested_types
            .iter_mut()
            .find(|e| e.name == *segment)?;
    }
    Some(current)
}

// Silence the unused-import linter: `PickledEntity` and `PickledType`
// appear only in doc comments today. We keep the explicit `use` so
// future merge logic (constraints, abbreviations) doesn't need to
// rediscover the imports.
#[allow(dead_code)]
fn _doc_uses(_e: &PickledEntity, _t: &PickledType) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_pickle::model::{
        CcuRef, FSharpTyparConstraint, IsType, Measure, Nullness, PickledAccess,
        PickledArgReprInfo, PickledCPath, PickledConst, PickledExnRepr, PickledHeader,
        PickledILScopeRef, PickledIdent, PickledMemberFlags, PickledMemberInfo, PickledMemberKind,
        PickledModulType, PickledNleRef, PickledOsgnTables, PickledParentRef, PickledPos,
        PickledRange, PickledTcAug, PickledTcRef, PickledTyconObjModelData,
        PickledTyconObjModelKind, PickledTyconRepr, PickledTyparReprInfo, PickledTyparSpecData,
        PickledType, PickledUCaseRef, PickledVal, PickledValReprInfo, PickledXmlDoc, TupleKind,
        TyparKind,
    };
    use crate::model::{
        AbbreviationTarget, Access, AssemblyIdentity, EntityKind, Member, MethodLike,
        MethodSignature, Nullability, Parameter, Primitive, TypeRef, Version,
    };
    use proptest::prelude::*;

    fn dummy_assembly() -> AssemblyIdentity {
        AssemblyIdentity {
            name: "Test".to_string(),
            version: Version {
                major: 1,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: None,
        }
    }

    fn dummy_range() -> PickledRange {
        PickledRange {
            file: 0,
            start: PickledPos { line: 0, column: 0 },
            end: PickledPos { line: 0, column: 0 },
        }
    }

    fn empty_modul_typ() -> PickledModulType {
        PickledModulType {
            is_type: IsType::Namespace,
            vals: Vec::new(),
            entities: Vec::new(),
        }
    }

    /// A module container (not a namespace): types declared inside it
    /// compile to *nested* CLR TypeDefs, so the ECMA projector places
    /// them under the module entity's `nested_types`.
    fn module_modul_typ(entities: Vec<u32>) -> PickledModulType {
        PickledModulType {
            is_type: IsType::ModuleOrType,
            vals: Vec::new(),
            entities,
        }
    }

    /// A `[<CompilationRepresentation(ModuleSuffix)>]` module container.
    /// Verified empirically: the pickle's `logical_name` for such a
    /// module already carries the `"Module"` suffix (e.g. `UnitsModule`),
    /// exactly matching the CLR TypeDef name the ECMA projector reads —
    /// so the merge looks up the same name on both sides.
    fn module_suffix_modul_typ(entities: Vec<u32>) -> PickledModulType {
        PickledModulType {
            is_type: IsType::FSharpModuleWithSuffix,
            vals: Vec::new(),
            entities,
        }
    }

    fn empty_tcaug() -> PickledTcAug {
        PickledTcAug {
            compare: None,
            compare_withc: None,
            hash_and_equals_withc: None,
            equals: None,
            adhoc: Vec::new(),
            interfaces: Vec::new(),
            super_type: None,
            is_abstract: false,
        }
    }

    fn make_entity(
        logical_name: &str,
        repr: PickledTyconRepr,
        modul_typ: PickledModulType,
    ) -> PickledEntity {
        make_entity_kinded(logical_name, repr, modul_typ, TyparKind::Type)
    }

    fn make_entity_kinded(
        logical_name: &str,
        repr: PickledTyconRepr,
        modul_typ: PickledModulType,
        typar_kind: TyparKind,
    ) -> PickledEntity {
        PickledEntity {
            typars: Vec::new(),
            logical_name: logical_name.to_string(),
            compiled_name: None,
            range: dummy_range(),
            pub_path: None,
            access: PickledAccess::new(),
            repr_access: PickledAccess::new(),
            attribs: Vec::new(),
            repr,
            type_abbrev: None,
            tcaug: empty_tcaug(),
            typar_kind,
            flags: 0,
            cpath: None,
            module_type: modul_typ,
            exn_repr: PickledExnRepr::None,
            xmldoc: None,
        }
    }

    fn make_ecma_entity(namespace: Vec<&str>, name: &str, kind: EntityKind) -> Entity {
        Entity {
            extension_member_names: Vec::new(),
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            is_extension_container: false,
            assembly: dummy_assembly(),
            namespace: namespace.into_iter().map(String::from).collect(),
            name: name.to_string(),
            kind,
            access: Access::Public,
            generic_parameters: Vec::new(),
            base_type: None,
            interfaces: Vec::new(),
            members: Vec::new(),
            skipped_members: Vec::new(),
            method_def_tokens: Vec::new(),
            is_sealed: false,
            nested_types: Vec::new(),
            is_readonly: false,
            is_byref_like: false,
            is_struct: false,
            is_auto_open: false,
            is_require_qualified_access: false,
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            obsolete: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: Vec::new(),
            source_name: None,
            custom_attrs: Vec::new(),
            abbreviation_target: None,
            definition_range: None,
        }
    }

    /// `FSharpObjectModel` repr matching what `[<Measure>] type m`
    /// actually pickles to in real F# DLLs: a `Class`-kind object
    /// model with no slots and no fields. The merge only consults
    /// `typar_kind` and the repr's *discriminant* (to confirm an
    /// ECMA TypeDef row exists), so we don't synthesise field data.
    fn measure_object_model_repr() -> PickledTyconRepr {
        PickledTyconRepr::FSharpObjectModel(PickledTyconObjModelData {
            kind: PickledTyconObjModelKind::Class,
            vslots: Vec::new(),
            rfields: Vec::new(),
        })
    }

    /// Build a minimal `PickledCcu` whose root entity wraps a
    /// `MiniLibFs` namespace fragment containing two measure types
    /// `m` and `kg`. The root mirrors real fsc output: a synthetic
    /// CCU wrapper whose `logical_name` matches the assembly name
    /// (`"MiniLibFs"`) and that contains a *separate* child entity
    /// named `"MiniLibFs"` for the user's `namespace MiniLibFs`
    /// declaration. Slot 0 is the wrapper, 1 is the namespace,
    /// 2/3 are the measures.
    fn ccu_with_two_measures() -> PickledCcu {
        let m = make_entity_kinded(
            "m",
            measure_object_model_repr(),
            empty_modul_typ(),
            TyparKind::Measure,
        );
        let kg = make_entity_kinded(
            "kg",
            measure_object_model_repr(),
            empty_modul_typ(),
            TyparKind::Measure,
        );
        let _ = Nullness::Ambivalent; // keep import used for doc-only refs
        let _ = PickledType::Var {
            typar_index: 0,
            nullness: Nullness::Ambivalent,
        };
        let mut minilibfs_modul = empty_modul_typ();
        minilibfs_modul.entities = vec![2, 3];
        let minilibfs = make_entity("MiniLibFs", PickledTyconRepr::NoRepr, minilibfs_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("MiniLibFs", PickledTyconRepr::NoRepr, root_modul);

        PickledCcu {
            header: PickledHeader {
                ccu_refs: vec![CcuRef {
                    name: "FSharp.Core".to_string(),
                }],
                ntycons: 4,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, minilibfs, m, kg],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        }
    }

    // -----------------------------------------------------------------------
    // Abbreviation-target decoder
    // -----------------------------------------------------------------------

    /// A `PickledModulType` for a real *type* (not a namespace fragment), so the
    /// walk records it in [`local_tycon_fqns`] and a `Local` tcref resolves.
    fn type_modul_typ() -> PickledModulType {
        PickledModulType {
            is_type: IsType::ModuleOrType,
            vals: Vec::new(),
            entities: Vec::new(),
        }
    }

    /// A `PickledCcu` with the header tables the decoder reads: a `FSharp.Core`
    /// ccu ref, the `Microsoft.FSharp.Core.int` string path, three nlerefs (one
    /// well-formed, one with a dangling ccu index, one with a dangling string
    /// index), one simplety pointing at the good nleref, and a same-assembly
    /// `Point` type (stamp 2) under a `Demo` namespace (stamp 1) — so a
    /// `Local(2)` tcref resolves to `["Demo", "Point"]`.
    fn abbrev_test_ccu() -> PickledCcu {
        let point = make_entity("Point", PickledTyconRepr::NoRepr, type_modul_typ());
        let mut demo_modul = empty_modul_typ();
        demo_modul.entities = vec![2];
        let demo = make_entity("Demo", PickledTyconRepr::NoRepr, demo_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Demo", PickledTyconRepr::NoRepr, root_modul);
        PickledCcu {
            header: PickledHeader {
                ccu_refs: vec![CcuRef {
                    name: "FSharp.Core".to_string(),
                }],
                ntycons: 3,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: ["Microsoft", "FSharp", "Core", "int"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                pubpaths: Vec::new(),
                nlerefs: vec![
                    PickledNleRef {
                        ccu: 0,
                        path: vec![0, 1, 2, 3],
                    },
                    // nleref 1: dangling ccu index.
                    PickledNleRef {
                        ccu: 99,
                        path: vec![0],
                    },
                    // nleref 2: dangling string index.
                    PickledNleRef {
                        ccu: 0,
                        path: vec![99],
                    },
                ],
                simpletys: vec![0],
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, demo, point],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        }
    }

    fn alias_entity() -> PickledEntity {
        make_entity("Alias", PickledTyconRepr::NoRepr, empty_modul_typ())
    }

    const AMB: Nullness = Nullness::Ambivalent;

    fn int_target() -> AbbreviationTarget {
        AbbreviationTarget::Named {
            ccu: Some("FSharp.Core".to_string()),
            path: vec![
                "Microsoft".into(),
                "FSharp".into(),
                "Core".into(),
                "int".into(),
            ],
            args: Vec::new(),
        }
    }

    #[test]
    fn decode_resolves_nonlocal_appsimple_and_local_heads() {
        let ccu = abbrev_test_ccu();
        let local = local_tycon_fqns(&ccu).unwrap();
        let entity = alias_entity();

        // Nullary NonLocal App: the full nleref path plus the FSharp.Core ccu.
        let app = PickledType::App {
            tcref: PickledTcRef::NonLocal(0),
            args: Vec::new(),
            nullness: AMB,
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &app, &local).unwrap(),
            Some(int_target()),
        );

        // AppSimple resolves the same nleref through the simpletys table.
        let simple = PickledType::AppSimple {
            simpletyp_index: 0,
            nullness: AMB,
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &simple, &local).unwrap(),
            Some(int_target()),
        );

        // Nullary Local: a same-assembly path with `ccu = None`.
        let local_app = PickledType::App {
            tcref: PickledTcRef::Local(2),
            args: Vec::new(),
            nullness: AMB,
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &local_app, &local).unwrap(),
            Some(AbbreviationTarget::Named {
                ccu: None,
                path: vec!["Demo".into(), "Point".into()],
                args: Vec::new(),
            }),
        );
    }

    #[test]
    fn decode_resolves_typar_by_position_through_forall() {
        let ccu = abbrev_test_ccu();
        let local = local_tycon_fqns(&ccu).unwrap();
        let mut entity = alias_entity();
        // The typar *stamps* — position, not value, is what the target encodes.
        entity.typars = vec![7, 9];

        // Bare Var referencing the second typar → position 1.
        let var = PickledType::Var {
            typar_index: 9,
            nullness: AMB,
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &var, &local).unwrap(),
            Some(AbbreviationTarget::Var(1)),
        );

        // A `Forall` quantifier wraps the same body; its `Var` still resolves
        // against the entity's own typars → position 0.
        let forall = PickledType::Forall {
            typars: vec![7, 9],
            body: Box::new(PickledType::Var {
                typar_index: 7,
                nullness: AMB,
            }),
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &forall, &local).unwrap(),
            Some(AbbreviationTarget::Var(0)),
        );

        // An out-of-scope typar declines rather than fabricating a position.
        let oob = PickledType::Var {
            typar_index: 99,
            nullness: AMB,
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &oob, &local).unwrap(),
            None,
        );
    }

    #[test]
    fn decode_resolves_structural_and_generic_shapes() {
        let ccu = abbrev_test_ccu();
        let local = local_tycon_fqns(&ccu).unwrap();
        let entity = alias_entity();
        let int = || PickledType::AppSimple {
            simpletyp_index: 0,
            nullness: AMB,
        };

        // Generic instantiation (App with args): the head plus the decoded args.
        let generic = PickledType::App {
            tcref: PickledTcRef::NonLocal(0),
            args: vec![int()],
            nullness: AMB,
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &generic, &local).unwrap(),
            Some(AbbreviationTarget::Named {
                ccu: Some("FSharp.Core".to_string()),
                path: vec![
                    "Microsoft".into(),
                    "FSharp".into(),
                    "Core".into(),
                    "int".into(),
                ],
                args: vec![int_target()],
            }),
        );

        // Function `int -> int`.
        let func = PickledType::Fun {
            domain: Box::new(int()),
            range: Box::new(int()),
            nullness: AMB,
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &func, &local).unwrap(),
            Some(AbbreviationTarget::Fun(
                Box::new(int_target()),
                Box::new(int_target()),
            )),
        );

        // Reference tuple `int * int` and struct tuple `struct (int * int)`.
        for (kind, struct_kind) in [(TupleKind::Reference, false), (TupleKind::Struct, true)] {
            let tuple = PickledType::Tuple {
                kind,
                elems: vec![int(), int()],
            };
            assert_eq!(
                decode_abbreviation_target(&ccu, &entity, &tuple, &local).unwrap(),
                Some(AbbreviationTarget::Tuple {
                    struct_kind,
                    elems: vec![int_target(), int_target()],
                }),
            );
        }
    }

    #[test]
    fn decode_declines_measure_and_union_case_and_partial_args() {
        let ccu = abbrev_test_ccu();
        let local = local_tycon_fqns(&ccu).unwrap();
        let entity = alias_entity();
        // A Var not in the entity's (empty) typar scope — declines.
        let unmodellable = PickledType::Var {
            typar_index: 5,
            nullness: AMB,
        };
        let good = || PickledType::AppSimple {
            simpletyp_index: 0,
            nullness: AMB,
        };

        let declined = [
            PickledType::Measure(Measure::One),
            PickledType::UCase {
                ucref: PickledUCaseRef {
                    tcref: PickledTcRef::NonLocal(0),
                    case_name_index: 0,
                },
                args: Vec::new(),
            },
            // A generic app whose arg declines makes the *whole* shape decline —
            // fail-closed, never a partial arg list.
            PickledType::App {
                tcref: PickledTcRef::NonLocal(0),
                args: vec![unmodellable.clone()],
                nullness: AMB,
            },
            // A well-formed-arity tuple with an undecodable element declines too.
            PickledType::Tuple {
                kind: TupleKind::Reference,
                elems: vec![good(), unmodellable],
            },
            // Degenerate tuple arities (a crafted/corrupt pickle) decline on arity
            // even when every element decodes: an F# tuple has at least two.
            PickledType::Tuple {
                kind: TupleKind::Reference,
                elems: vec![],
            },
            PickledType::Tuple {
                kind: TupleKind::Struct,
                elems: vec![good()],
            },
        ];
        for ty in declined {
            assert_eq!(
                decode_abbreviation_target(&ccu, &entity, &ty, &local).unwrap(),
                None,
                "{ty:?} must decline",
            );
        }
    }

    #[test]
    fn decode_fails_loud_on_dangling_indices() {
        let ccu = abbrev_test_ccu();
        let local = local_tycon_fqns(&ccu).unwrap();
        let entity = alias_entity();

        let danglers = [
            // Dangling nleref index.
            PickledType::App {
                tcref: PickledTcRef::NonLocal(999),
                args: Vec::new(),
                nullness: AMB,
            },
            // Well-formed nleref, but its ccu index dangles (nleref 1).
            PickledType::App {
                tcref: PickledTcRef::NonLocal(1),
                args: Vec::new(),
                nullness: AMB,
            },
            // Well-formed nleref, but a path string index dangles (nleref 2).
            PickledType::App {
                tcref: PickledTcRef::NonLocal(2),
                args: Vec::new(),
                nullness: AMB,
            },
            // Dangling simplety index.
            PickledType::AppSimple {
                simpletyp_index: 999,
                nullness: AMB,
            },
            // Local stamp beyond the tycon table.
            PickledType::App {
                tcref: PickledTcRef::Local(999),
                args: Vec::new(),
                nullness: AMB,
            },
        ];
        for ty in danglers {
            assert!(
                matches!(
                    decode_abbreviation_target(&ccu, &entity, &ty, &local),
                    Err(ImportError::OsgnIndexOutOfRange { .. })
                ),
                "{ty:?} must fail loud as OsgnIndexOutOfRange",
            );
        }
    }

    #[test]
    fn decode_declines_in_range_local_that_is_not_a_type() {
        // `Local(1)` is the `Demo` namespace fragment — in range, but not a
        // recorded module/type. A well-formed shape we do not model: decline,
        // do not fail loud.
        let ccu = abbrev_test_ccu();
        let local = local_tycon_fqns(&ccu).unwrap();
        let entity = alias_entity();
        let ns_ref = PickledType::App {
            tcref: PickledTcRef::Local(1),
            args: Vec::new(),
            nullness: AMB,
        };
        assert_eq!(
            decode_abbreviation_target(&ccu, &entity, &ns_ref, &local).unwrap(),
            None,
        );
    }

    fn arb_nullness() -> impl Strategy<Value = Nullness> {
        prop_oneof![
            Just(Nullness::Ambivalent),
            Just(Nullness::WithNull),
            Just(Nullness::WithoutNull),
        ]
    }

    fn arb_tcref() -> impl Strategy<Value = PickledTcRef> {
        prop_oneof![
            (0u32..2000).prop_map(PickledTcRef::Local),
            (0u32..2000).prop_map(PickledTcRef::NonLocal),
        ]
    }

    /// Arbitrary well-typed `PickledType` trees over the decoder-relevant shapes,
    /// with indices spanning both in-range and dangling values so the property
    /// exercises every table lookup and every fail-loud path.
    fn arb_pickled_type() -> impl Strategy<Value = PickledType> {
        let leaf = prop_oneof![
            (0u32..2000, arb_nullness()).prop_map(|(typar_index, nullness)| PickledType::Var {
                typar_index,
                nullness
            }),
            (0u32..2000, arb_nullness()).prop_map(|(simpletyp_index, nullness)| {
                PickledType::AppSimple {
                    simpletyp_index,
                    nullness,
                }
            }),
            Just(PickledType::Measure(Measure::One)),
        ];
        leaf.prop_recursive(4, 48, 4, |inner| {
            prop_oneof![
                (
                    arb_tcref(),
                    prop::collection::vec(inner.clone(), 0..3),
                    arb_nullness()
                )
                    .prop_map(|(tcref, args, nullness)| PickledType::App {
                        tcref,
                        args,
                        nullness
                    }),
                (inner.clone(), inner.clone(), arb_nullness()).prop_map(
                    |(domain, range, nullness)| PickledType::Fun {
                        domain: Box::new(domain),
                        range: Box::new(range),
                        nullness,
                    }
                ),
                prop::collection::vec(inner.clone(), 0..3).prop_map(|elems| PickledType::Tuple {
                    kind: TupleKind::Reference,
                    elems
                }),
                (prop::collection::vec(0u32..2000, 0..3), inner).prop_map(|(typars, body)| {
                    PickledType::Forall {
                        typars,
                        body: Box::new(body),
                    }
                }),
            ]
        })
    }

    proptest! {
        /// Totality + fail-loud-only-for-malformed: the decoder never panics on an
        /// arbitrary (possibly dangling) `PickledType`, and the only error it may
        /// return is a loud `OsgnIndexOutOfRange`. No panic mid-walk means the
        /// crate's fail-loud contract holds even for adversarial pickle input; a
        /// clean `Ok(None)` for every unmodelled-but-well-formed shape means the
        /// decode is fail-closed, never partial.
        #[test]
        fn decode_is_total_and_fails_only_loud(ty in arb_pickled_type()) {
            let ccu = abbrev_test_ccu();
            let local = local_tycon_fqns(&ccu).unwrap();
            let mut entity = alias_entity();
            entity.typars = vec![0, 1, 2];
            match decode_abbreviation_target(&ccu, &entity, &ty, &local) {
                Ok(_) => {}
                Err(ImportError::OsgnIndexOutOfRange { .. }) => {}
                Err(other) => prop_assert!(false, "unexpected non-index error: {other:?}"),
            }
        }
    }

    #[test]
    fn upgrades_class_to_measure_on_fqn_match() {
        let pickled = ccu_with_two_measures();
        let mut entities = vec![
            make_ecma_entity(vec!["MiniLibFs"], "m", EntityKind::Class),
            make_ecma_entity(vec!["MiniLibFs"], "kg", EntityKind::Class),
            // Unrelated entity that must not be touched.
            make_ecma_entity(vec!["MiniLibFs"], "Point", EntityKind::Record),
        ];

        merge_measure_entities(&mut entities, &pickled).expect("merge");

        assert_eq!(entities[0].kind, EntityKind::Measure);
        assert_eq!(entities[1].kind, EntityKind::Measure);
        // Unrelated entity is unchanged.
        assert_eq!(entities[2].kind, EntityKind::Record);
    }

    #[test]
    fn errors_when_pickled_measure_has_no_ecma_row() {
        let pickled = ccu_with_two_measures();
        // ECMA tree is missing `kg`.
        let mut entities = vec![make_ecma_entity(vec!["MiniLibFs"], "m", EntityKind::Class)];

        let err = merge_measure_entities(&mut entities, &pickled).unwrap_err();
        match err {
            ImportError::FsharpPickleMergeMismatch { detail } => {
                assert!(detail.contains("MiniLibFs.kg"), "got: {detail}");
                assert!(detail.contains("no matching ECMA"), "got: {detail}");
            }
            other => panic!("expected FsharpPickleMergeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn errors_when_ecma_kind_is_not_class() {
        let pickled = ccu_with_two_measures();
        let mut entities = vec![
            make_ecma_entity(vec!["MiniLibFs"], "m", EntityKind::Struct),
            make_ecma_entity(vec!["MiniLibFs"], "kg", EntityKind::Class),
        ];

        let err = merge_measure_entities(&mut entities, &pickled).unwrap_err();
        match err {
            ImportError::FsharpPickleMergeMismatch { detail } => {
                assert!(detail.contains("MiniLibFs.m"), "got: {detail}");
                assert!(detail.contains("Struct"), "got: {detail}");
            }
            other => panic!("expected FsharpPickleMergeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn idempotent_on_already_merged_tree() {
        let pickled = ccu_with_two_measures();
        let mut entities = vec![
            make_ecma_entity(vec!["MiniLibFs"], "m", EntityKind::Measure),
            make_ecma_entity(vec!["MiniLibFs"], "kg", EntityKind::Measure),
        ];

        merge_measure_entities(&mut entities, &pickled).expect("idempotent merge");
        assert_eq!(entities[0].kind, EntityKind::Measure);
        assert_eq!(entities[1].kind, EntityKind::Measure);
    }

    /// Regression for the P1 fix from codex review: a measure entity
    /// whose repr is `Measureable` (the `[<Measure>] type T = m * kg`
    /// abbreviation form, or the FSharp.Core
    /// `[<MeasureAnnotatedAbbreviation>] type float<[<Measure>]
    /// 'Measure> = float` family) carries `typar_kind = Measure` but
    /// has no backing ECMA TypeDef. The merge must skip it rather
    /// than demanding an ECMA entry and erroring.
    #[test]
    fn ignores_measure_abbreviation_with_no_ecma_row() {
        // Same shape as `ccu_with_two_measures` but `m` is replaced
        // by a measure abbreviation (`Measureable` repr).
        let m = make_entity_kinded(
            "m",
            PickledTyconRepr::Measureable(PickledType::Var {
                typar_index: 0,
                nullness: Nullness::Ambivalent,
            }),
            empty_modul_typ(),
            TyparKind::Measure,
        );
        let kg = make_entity_kinded(
            "kg",
            measure_object_model_repr(),
            empty_modul_typ(),
            TyparKind::Measure,
        );
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2, 3];
        let ns = make_entity("MiniLibFs", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("MiniLibFs", PickledTyconRepr::NoRepr, root_modul);
        let pickled = PickledCcu {
            header: PickledHeader {
                ccu_refs: Vec::new(),
                ntycons: 4,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, ns, m, kg],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        };

        // ECMA tree has no `m` (the abbreviation isn't emitted) and a
        // real Class for `kg`. Merge must succeed — `m` is skipped
        // even though `typar_kind = Measure`, and `kg` is upgraded.
        let mut entities = vec![make_ecma_entity(vec!["MiniLibFs"], "kg", EntityKind::Class)];
        merge_measure_entities(&mut entities, &pickled).expect("merge skips abbreviation");
        assert_eq!(entities[0].kind, EntityKind::Measure);
    }

    /// Regression for the structure observed in real fsc output and
    /// missed by the initial walker: the synthetic CCU root's
    /// `logical_name` matches the assembly name and must be
    /// suppressed from the FQN path. For MiniLibFs the root is named
    /// `"MiniLibFs"` and contains a *separate* child entity also
    /// named `"MiniLibFs"` (the user's `namespace MiniLibFs`
    /// declaration). A name-emptiness heuristic would produce
    /// `"MiniLibFs.MiniLibFs.m"` here; the explicit `is_root` gate
    /// produces `"MiniLibFs.m"`.
    #[test]
    fn suppresses_named_root_entity_from_fqn_path() {
        let pickled = ccu_with_two_measures();
        // Root is named "MiniLibFs" (mirrors real fsc output); the
        // namespace fragment immediately under it is also named
        // "MiniLibFs". If the walker doubled the segment we'd be
        // looking up `MiniLibFs.MiniLibFs.m` which doesn't exist in
        // the ECMA tree, and the merge would error.
        assert_eq!(
            pickled.tables.tycons[pickled.root_entity as usize].logical_name,
            "MiniLibFs"
        );
        let mut entities = vec![
            make_ecma_entity(vec!["MiniLibFs"], "m", EntityKind::Class),
            make_ecma_entity(vec!["MiniLibFs"], "kg", EntityKind::Class),
        ];
        merge_measure_entities(&mut entities, &pickled).expect("merge with named root");
        assert_eq!(entities[0].kind, EntityKind::Measure);
        assert_eq!(entities[1].kind, EntityKind::Measure);
    }

    /// Regression for codex P1: a measure declared inside an F# module
    /// (`module Units = [<Measure>] type m`). The module compiles to a
    /// CLR class, so the ECMA projector places the measure TypeDef in
    /// the module entity's `nested_types` with an *empty* namespace.
    /// The pickle walk yields the flat path `MyApp.Units.m`; the merge
    /// must split this into the top-level entity `(["MyApp"], "Units")`
    /// and descend into its nested types to find `m`. A flat top-level
    /// scan would miss it and wrongly reject a valid assembly.
    #[test]
    fn upgrades_module_scoped_measure_in_nested_types() {
        // Pickle: root(CCU) -> namespace "MyApp" -> module "Units" -> measure "m".
        let m = make_entity_kinded(
            "m",
            measure_object_model_repr(),
            empty_modul_typ(),
            TyparKind::Measure,
        );
        let units = make_entity("Units", PickledTyconRepr::NoRepr, module_modul_typ(vec![3]));
        let mut myapp_modul = empty_modul_typ();
        myapp_modul.entities = vec![2];
        let myapp = make_entity("MyApp", PickledTyconRepr::NoRepr, myapp_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let pickled = PickledCcu {
            header: PickledHeader {
                ccu_refs: Vec::new(),
                ntycons: 4,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, myapp, units, m],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        };

        // ECMA: top-level module "Units" in namespace ["MyApp"], with a
        // nested measure type "m" carrying an empty namespace.
        let mut units_ecma = make_ecma_entity(vec!["MyApp"], "Units", EntityKind::Module);
        units_ecma
            .nested_types
            .push(make_ecma_entity(vec![], "m", EntityKind::Class));
        let mut entities = vec![units_ecma];

        merge_measure_entities(&mut entities, &pickled).expect("merge module-scoped measure");
        assert_eq!(entities[0].kind, EntityKind::Module);
        assert_eq!(entities[0].nested_types[0].kind, EntityKind::Measure);
    }

    #[test]
    fn upgrades_module_suffix_scoped_measure_using_clr_name() {
        // `[<CompilationRepresentation(ModuleSuffix)>] module Units`
        // containing `[<Measure>] type m`. The pickle marks the module
        // container `FSharpModuleWithSuffix`, but its `logical_name` is
        // already the CLR name `"UnitsModule"` (empirically confirmed) —
        // the same name `build_type_tree` emits for the TypeDef. The
        // merge must therefore find `ModSuffix.UnitsModule.m` and upgrade
        // it without any name-suffix special-casing.
        let m = make_entity_kinded(
            "m",
            measure_object_model_repr(),
            empty_modul_typ(),
            TyparKind::Measure,
        );
        let units = make_entity(
            "UnitsModule",
            PickledTyconRepr::NoRepr,
            module_suffix_modul_typ(vec![3]),
        );
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2];
        let ns = make_entity("ModSuffix", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("ModSuffix", PickledTyconRepr::NoRepr, root_modul);
        let pickled = PickledCcu {
            header: PickledHeader {
                ccu_refs: Vec::new(),
                ntycons: 4,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, ns, units, m],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        };

        // ECMA: `build_type_tree` emits the module TypeDef as
        // `ModSuffix.UnitsModule` with the measure nested under it.
        let mut units_ecma = make_ecma_entity(vec!["ModSuffix"], "UnitsModule", EntityKind::Module);
        units_ecma
            .nested_types
            .push(make_ecma_entity(vec![], "m", EntityKind::Class));
        let mut entities = vec![units_ecma];

        merge_measure_entities(&mut entities, &pickled).expect("merge module-suffix measure");
        assert_eq!(entities[0].kind, EntityKind::Module);
        assert_eq!(entities[0].nested_types[0].kind, EntityKind::Measure);
    }

    #[test]
    fn matches_measure_by_compiled_name_not_logical_name() {
        // `[<CompiledName("Metre")>] [<Measure>] type m` in
        // `namespace MiniLibFs`. The pickle carries `logical_name = "m"`
        // and `compiled_name = Some("Metre")`; the ECMA TypeDef is named
        // `Metre`. The merge must key off the compiled name to find it.
        let mut m = make_entity_kinded(
            "m",
            measure_object_model_repr(),
            empty_modul_typ(),
            TyparKind::Measure,
        );
        m.compiled_name = Some("Metre".to_string());
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2];
        let ns = make_entity("MiniLibFs", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("MiniLibFs", PickledTyconRepr::NoRepr, root_modul);
        let pickled = PickledCcu {
            header: PickledHeader {
                ccu_refs: Vec::new(),
                ntycons: 3,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, ns, m],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        };

        // ECMA tree is keyed by the compiled name only.
        let mut entities = vec![make_ecma_entity(
            vec!["MiniLibFs"],
            "Metre",
            EntityKind::Class,
        )];
        merge_measure_entities(&mut entities, &pickled).expect("merge by compiled name");
        assert_eq!(entities[0].kind, EntityKind::Measure);
    }

    #[test]
    fn overlay_propagates_genuine_mismatch() {
        // A *successfully decoded* pickle that disagrees with the ECMA
        // tree (here: a measure with no matching TypeDef) is fatal.
        let pickled = ccu_with_two_measures();
        let mut entities = vec![make_ecma_entity(vec!["MiniLibFs"], "m", EntityKind::Class)];
        let err = apply_measure_overlay(&mut entities, &pickled)
            .expect_err("missing kg must surface as a hard mismatch");
        assert!(matches!(err, ImportError::FsharpPickleMergeMismatch { .. }));
    }

    #[test]
    fn overlay_upgrades_on_valid_decode() {
        let pickled = ccu_with_two_measures();
        let mut entities = vec![
            make_ecma_entity(vec!["MiniLibFs"], "m", EntityKind::Class),
            make_ecma_entity(vec!["MiniLibFs"], "kg", EntityKind::Class),
        ];
        apply_measure_overlay(&mut entities, &pickled).expect("valid decode merges");
        assert_eq!(entities[0].kind, EntityKind::Measure);
        assert_eq!(entities[1].kind, EntityKind::Measure);
    }

    #[test]
    fn ignores_non_measure_typar_kind() {
        // Build a CCU where the only entity is a record with
        // `typar_kind = Type`. Merge must leave the ECMA tree
        // untouched and succeed.
        let record = make_entity(
            "Point",
            PickledTyconRepr::Record(Vec::new()),
            empty_modul_typ(),
        );
        let mut minilibfs_modul = empty_modul_typ();
        minilibfs_modul.entities = vec![2];
        let minilibfs = make_entity("MiniLibFs", PickledTyconRepr::NoRepr, minilibfs_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("", PickledTyconRepr::NoRepr, root_modul);
        let pickled = PickledCcu {
            header: PickledHeader {
                ccu_refs: Vec::new(),
                ntycons: 3,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, minilibfs, record],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        };

        let mut entities = vec![make_ecma_entity(
            vec!["MiniLibFs"],
            "Point",
            EntityKind::Record,
        )];
        merge_measure_entities(&mut entities, &pickled).expect("merge");
        assert_eq!(entities[0].kind, EntityKind::Record);
    }

    /// A minimal `PickledVal` exercising only the fields
    /// [`module_extension_candidates`] reads (`compiled_name`, `flags`,
    /// `member_info.flags.is_instance`); everything else is inert, and
    /// `repr_info` is `None` so [`val_il_arity`] yields `None`.
    /// `member_instance` is `None` for a plain `let` (no `member_info`).
    fn ext_test_val(
        compiled_name: Option<&str>,
        flags: i64,
        member_instance: Option<bool>,
    ) -> PickledVal {
        PickledVal {
            logical_name: "v".to_string(),
            compiled_name: compiled_name.map(str::to_string),
            range: None,
            other_range: None,
            ty: PickledType::Tuple {
                kind: TupleKind::Reference,
                elems: Vec::new(),
            },
            flags,
            member_info: member_instance.map(|is_instance| PickledMemberInfo {
                apparent_parent: PickledTcRef::Local(0),
                flags: PickledMemberFlags {
                    is_instance,
                    is_dispatch_slot: false,
                    is_override_or_explicit_impl: false,
                    is_final: false,
                    kind: PickledMemberKind::Member,
                },
                implemented_slots: Vec::new(),
                is_implemented: true,
            }),
            attribs: Vec::new(),
            repr_info: None,
            xmldoc_sig: String::new(),
            access: PickledAccess::new(),
            parent: PickledParentRef::None,
            literal_value: None,
            xmldoc: None,
        }
    }

    /// A `ValReprInfo` whose argument groups have the given lengths — the only
    /// part [`val_il_arity`] consults. The compiled IL arity equals the sum of
    /// these lengths. For an instance member the first group is the
    /// re-prepended receiver (`&[1, …]`); a trailing unit argument is a
    /// zero-length group (`&[1, 0]` for `member this.M()`).
    fn val_repr_info(arg_groups: &[usize]) -> PickledValReprInfo {
        let arg = || PickledArgReprInfo {
            attribs: Vec::new(),
            name: None,
        };
        PickledValReprInfo {
            typar_repr: Vec::new(),
            arg_repr: arg_groups
                .iter()
                .map(|&n| (0..n).map(|_| arg()).collect())
                .collect(),
            return_repr: arg(),
        }
    }

    /// As [`ext_test_val`], but carrying a `ValReprInfo` with the given argument
    /// groups so [`val_il_arity`] returns `Some(sum-of-lengths)`.
    fn ext_test_val_arity(
        compiled_name: Option<&str>,
        flags: i64,
        member_instance: Option<bool>,
        arg_groups: &[usize],
    ) -> PickledVal {
        let mut v = ext_test_val(compiled_name, flags, member_instance);
        v.repr_info = Some(val_repr_info(arg_groups));
        v
    }

    /// A `PickledCcu` carrying the given tycon and val tables; header counts
    /// are derived, everything else inert.
    fn make_ccu(tycons: Vec<PickledEntity>, vals: Vec<PickledVal>, root_entity: u32) -> PickledCcu {
        PickledCcu {
            header: PickledHeader {
                ccu_refs: Vec::new(),
                ntycons: tycons.len() as u32,
                ntypars: 0,
                nvals: vals.len() as u32,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons,
                typars: Vec::new(),
                vals,
            },
        }
    }

    /// A module container holding the given val osgn indices.
    fn module_with_vals(vals: Vec<u32>) -> PickledModulType {
        PickledModulType {
            is_type: IsType::ModuleOrType,
            vals,
            entities: Vec::new(),
        }
    }

    /// A minimal projected static method with `arity` void parameters,
    /// unflagged as an extension.
    fn make_ecma_method_arity(name: &str, arity: usize) -> Member {
        let mut m = make_ecma_method(name);
        if let Member::Method(mm) = &mut m {
            mm.signature.parameters = (0..arity)
                .map(|_| Parameter {
                    name: None,
                    ty: TypeRef::Primitive(Primitive::I4),
                    is_byref: false,
                    is_out: false,
                    is_readonly_ref: false,
                    default: crate::model::ParamDefault::None,
                    is_param_array: false,
                    nullability: Nullability::Oblivious,
                })
                .collect();
        }
        m
    }

    /// A public static **literal** field — the shape fsc emits for `[<Literal>] let`.
    fn make_ecma_literal_field(name: &str) -> Member {
        Member::Field(crate::model::Field {
            name: name.to_string(),
            access: Access::Public,
            ty: crate::model::TypeRef::Primitive(crate::model::Primitive::I4),
            is_static: true,
            is_init_only: false,
            is_literal: true,
            is_volatile: false,
            is_required: false,
            compiler_feature_required: Vec::new(),
            nullability: crate::model::Nullability::Oblivious,
            custom_attrs: Vec::new(),
        })
    }

    /// A minimal projected static method (zero parameters), unflagged as an
    /// extension.
    fn make_ecma_method(name: &str) -> Member {
        Member::Method(MethodLike {
            definition_range: None,
            name: name.to_string(),
            access: Access::Public,
            signature: MethodSignature {
                parameters: Vec::new(),
                return_type: TypeRef::Primitive(Primitive::Void),
                return_nullability: Nullability::Oblivious,
            },
            arg_group_count: Some(1),
            is_static: true,
            is_virtual: false,
            is_abstract: false,
            is_constructor: false,
            module_value: None,
            is_module_value_binding: false,
            is_extension_method: false,
            augmentation: Augmentation::No,
            is_final: false,
            is_newslot: false,
            is_hide_by_sig: false,
            generic_parameters: Vec::new(),
            obsolete: None,
            experimental: None,
            sets_required_members: false,
            compiler_feature_required: Vec::new(),
            source_name: None,
            custom_attrs: Vec::new(),
            metadata_token: 0,
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        })
    }

    fn is_extension(entity: &Entity, method_name: &str) -> bool {
        entity.members.iter().any(
            |m| matches!(m, Member::Method(mm) if mm.name == method_name && mm.is_extension_method),
        )
    }

    /// A single-module CCU (root → NS → module `M` holding the given vals) and
    /// its ECMA counterpart carrying the given members; returns the entity
    /// after [`apply_module_member_projection`] rebuilt its member list.
    fn run_member_projection(vals: Vec<PickledVal>, members: Vec<Member>) -> Entity {
        let val_indices: Vec<u32> = (0..vals.len() as u32).collect();
        let module = make_entity("M", PickledTyconRepr::NoRepr, module_with_vals(val_indices));
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2];
        let ns = make_entity("NS", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, ns, module], vals, 0);

        let mut entities = vec![{
            let mut e = make_ecma_entity(vec!["NS"], "M", EntityKind::Module);
            e.members = members;
            e
        }];
        apply_module_member_projection(&mut entities, &ccu).expect("member projection");
        entities.into_iter().next().expect("one entity")
    }

    /// The FCS `IsInstanceMember` gate on the surface extension flag, applied
    /// per claimed member: an instance augmentation is flagged; a *static*
    /// augmentation (carries `IsExtensionMember` but not `IsInstance`) and a
    /// plain dotted-`[<CompiledName>]` `let` are not — the false positive the
    /// IL-name heuristic produced. A *generic* instance extension is flagged
    /// too: the §7 gap the retired per-method overlay could not close (it had
    /// to skip generic vals; the val-driven list claims their generic IL
    /// methods like any other).
    #[test]
    fn member_list_flags_instance_extensions_only() {
        let ext = VAL_FLAGS_IS_EXTENSION_MEMBER;
        let mut generic_ext = ext_test_val_arity(Some("Counter.Doubled"), ext, Some(true), &[1, 1]);
        generic_ext
            .repr_info
            .as_mut()
            .expect("arity helper sets repr_info")
            .typar_repr
            .push(PickledTyparReprInfo {
                ident: PickledIdent {
                    name: "T".to_string(),
                    range: dummy_range(),
                },
                kind: TyparKind::Type,
            });
        let entity = run_member_projection(
            vec![
                ext_test_val_arity(Some("Counter.Tripled"), ext, Some(true), &[1, 0]),
                ext_test_val(Some("Counter.Make.Static"), ext, Some(false)),
                ext_test_val(Some("A.B"), 0, None),
                generic_ext,
            ],
            vec![
                make_ecma_method_arity("Counter.Tripled", 1),
                make_ecma_method("Counter.Make.Static"),
                make_ecma_method("A.B"),
                make_ecma_method_arity("Counter.Doubled", 2),
            ],
        );
        assert!(is_extension(&entity, "Counter.Tripled"));
        assert!(!is_extension(&entity, "Counter.Make.Static"));
        assert!(!is_extension(&entity, "A.B"));
        assert!(
            is_extension(&entity, "Counter.Doubled"),
            "a generic instance extension val must flag its claimed member"
        );
        assert!(entity.skipped_members.is_empty(), "every val claimed");
    }

    /// Pins [`val_il_arity`]'s unit handling: fsc's `ValReprInfo` already
    /// encodes unit erasure, so summing argument-group lengths yields the true
    /// MethodDef arity with no unit special-casing. Empirically verified against
    /// net10.0 fsc output: a *non-erased* `()` group (curried alongside another
    /// argument) pickles as a length-1 group and emits a real `Unit` IL
    /// parameter, while only the *erased* lone `()` pickles as a zero-length
    /// group. (The naive "an empty group means an erased unit" reading would
    /// under-count `let g () x = x` as arity 1; the pickle says arity 2.)
    #[test]
    fn val_il_arity_counts_non_erased_unit_groups() {
        let arity =
            |groups: &[usize]| val_il_arity(&ext_test_val_arity(Some("v"), 0, None, groups));
        assert_eq!(arity(&[0]), Some(0)); // `let a () = 1`              -> IL ()           arity 0
        assert_eq!(arity(&[1, 1]), Some(2)); // `let b () x = x`         -> IL (Unit, Int32) arity 2
        assert_eq!(arity(&[1, 0]), Some(1)); // `member this.M () = …`   -> IL (receiver)    arity 1
        assert_eq!(arity(&[1, 1, 1]), Some(3)); // `member this.M () x`  -> IL (recv,Unit,I) arity 3
        // No `ValReprInfo` → arity unknown (can witness a collision, not break it).
        assert_eq!(val_il_arity(&ext_test_val(Some("v"), 0, None)), None);
    }

    /// Pins the per-val projection of [`collect_module_member_targets`]: the
    /// index keeps every val of the module, in pickle order, and each entry
    /// carries the six facts with their documented provenance — including the
    /// no-compiled-name and generic vals the IL-matching overlay collectors
    /// deliberately skip.
    #[test]
    fn module_member_index_projects_vals_in_order() {
        let ext = VAL_FLAGS_IS_EXTENSION_MEMBER;
        // A `[<CompiledName("Tripled")>] let tripled x = …`.
        let mut renamed = ext_test_val_arity(Some("Tripled"), 0, None, &[1]);
        renamed.logical_name = "tripled".to_string();
        // A `type Counter with member this.Doubled() = …` instance augmentation.
        let mut instance_ext =
            ext_test_val_arity(Some("Counter.Doubled"), ext, Some(true), &[1, 0]);
        instance_ext.logical_name = "Doubled".to_string();
        // A `type Counter with static member Make …` — carries the extension
        // bit but is not an instance member.
        let mut static_ext = ext_test_val(Some("Counter.Make.Static"), ext, Some(false));
        static_ext.logical_name = "Make".to_string();
        // A generic `let id x = x` — non-empty `typar_repr`, no compiled name.
        let mut generic_let = ext_test_val_arity(None, 0, None, &[1]);
        generic_let.logical_name = "id".to_string();
        generic_let
            .repr_info
            .as_mut()
            .expect("arity helper sets repr_info")
            .typar_repr
            .push(PickledTyparReprInfo {
                ident: PickledIdent {
                    name: "T".to_string(),
                    range: dummy_range(),
                },
                kind: TyparKind::Type,
            });
        // No `ValReprInfo` and no compiled name at all.
        let mut bare = ext_test_val(None, 0, None);
        bare.logical_name = "helper".to_string();

        let module = make_entity(
            "M",
            PickledTyconRepr::NoRepr,
            module_with_vals(vec![0, 1, 2, 3, 4]),
        );
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2];
        let ns = make_entity("NS", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(
            vec![root, ns, module],
            vec![renamed, instance_ext, static_ext, generic_let, bare],
            0,
        );

        let targets = collect_module_member_targets(&ccu).expect("collect");
        assert_eq!(targets.len(), 1);
        let target = &targets[0];
        assert_eq!(target.namespace, ["NS"]);
        assert_eq!(target.type_chain, ["M"]);
        assert_eq!(
            target.vals,
            vec![
                ModuleMemberVal {
                    val_index: 0,
                    logical_name: "tripled".to_string(),
                    compiled_name: Some("Tripled".to_string()),
                    il_arity: Some(1),
                    arg_group_count: Some(1),
                    is_instance: false,
                    is_member: false,
                    is_extension: false,
                    is_generic: false,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                },
                ModuleMemberVal {
                    val_index: 1,
                    logical_name: "Doubled".to_string(),
                    compiled_name: Some("Counter.Doubled".to_string()),
                    // Receiver group + erased-unit group.
                    il_arity: Some(1),
                    arg_group_count: Some(2),
                    is_instance: true,
                    is_member: true,
                    is_extension: true,
                    is_generic: false,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                },
                ModuleMemberVal {
                    val_index: 2,
                    logical_name: "Make".to_string(),
                    compiled_name: Some("Counter.Make.Static".to_string()),
                    il_arity: None,
                    arg_group_count: None,
                    is_instance: false,
                    is_member: true,
                    is_extension: true,
                    is_generic: false,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                },
                ModuleMemberVal {
                    val_index: 3,
                    logical_name: "id".to_string(),
                    compiled_name: None,
                    il_arity: Some(1),
                    arg_group_count: Some(1),
                    is_instance: false,
                    is_member: false,
                    is_extension: false,
                    is_generic: true,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                },
                ModuleMemberVal {
                    val_index: 4,
                    logical_name: "helper".to_string(),
                    compiled_name: None,
                    il_arity: None,
                    arg_group_count: None,
                    is_instance: false,
                    is_member: false,
                    is_extension: false,
                    is_generic: false,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                },
            ]
        );
        // The IL cross-reference key: explicit compiled name when present,
        // else the logical name.
        assert_eq!(target.vals[0].il_name(), "Tripled");
        assert_eq!(target.vals[3].il_name(), "id");
    }

    /// Pins the index's FQN construction and entity coverage: a nested
    /// suffix-module extends the type chain (not the namespace); a val-less
    /// module and a record both still get a target with empty `vals` (the
    /// presence signal the cutover's missing-module policy relies on — a
    /// record's member vals live in `tcaug.adhoc`, not `module_type.vals`).
    #[test]
    fn module_member_index_records_nested_and_val_less_entities() {
        let mut v = ext_test_val_arity(None, 0, None, &[1]);
        v.logical_name = "inner".to_string();

        let inner = make_entity(
            "InnerModule",
            PickledTyconRepr::NoRepr,
            PickledModulType {
                is_type: IsType::FSharpModuleWithSuffix,
                vals: vec![0],
                entities: Vec::new(),
            },
        );
        let record = make_entity(
            "Point",
            PickledTyconRepr::Record(Vec::new()),
            module_modul_typ(Vec::new()),
        );
        let outer = make_entity(
            "Outer",
            PickledTyconRepr::NoRepr,
            module_modul_typ(vec![2, 3]),
        );
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, outer, inner, record], vec![v], 0);

        let targets = collect_module_member_targets(&ccu).expect("collect");
        let by_chain: Vec<(&[String], usize)> = targets
            .iter()
            .map(|t| (t.type_chain.as_slice(), t.vals.len()))
            .collect();
        assert_eq!(
            by_chain,
            [
                (&["Outer".to_string()][..], 0),
                (&["Outer".to_string(), "InnerModule".to_string()][..], 1),
                (&["Outer".to_string(), "Point".to_string()][..], 0),
            ]
        );
        assert!(targets.iter().all(|t| t.namespace.is_empty()));
        assert_eq!(targets[1].vals[0].logical_name, "inner");
    }

    /// Pins the IL-shape facts the review of the naive index caught missing:
    /// a *value* binding (zero argument groups → a static property, or a
    /// literal field) shares `il_arity = Some(0)` with a unit-taking
    /// *function* (one erased-unit group → a zero-parameter MethodDef) and
    /// only `arg_group_count` separates them; and a typar generic only over a
    /// *measure* is erased from IL, so it must not set `is_generic`
    /// (empirically: `LanguagePrimitives.FloatWithMeasure` pickles one
    /// measure typar yet its MethodDef has zero generic parameters).
    #[test]
    fn module_member_index_distinguishes_values_functions_and_erased_measures() {
        let typar = |kind| PickledTyparReprInfo {
            ident: PickledIdent {
                name: "T".to_string(),
                range: dummy_range(),
            },
            kind,
        };
        // `let answer = 42` — zero curried groups.
        let mut value = ext_test_val_arity(None, 0, None, &[]);
        value.logical_name = "answer".to_string();
        // `let f () = …` — one erased-unit (zero-length) group.
        let mut unit_fn = ext_test_val_arity(None, 0, None, &[0]);
        unit_fn.logical_name = "f".to_string();
        // `[<Literal>] let RequiresPreview = "…"` — a value with a constant.
        let mut literal = ext_test_val_arity(None, 0, None, &[]);
        literal.logical_name = "RequiresPreview".to_string();
        literal.literal_value = Some(PickledConst::Int32(42));
        // `let FloatWithMeasure (f: float) : float<'m> = …` — measure-only
        // typar, erased from IL.
        let mut measure_only = ext_test_val_arity(None, 0, None, &[1]);
        measure_only.logical_name = "FloatWithMeasure".to_string();
        measure_only
            .repr_info
            .as_mut()
            .expect("arity helper sets repr_info")
            .typar_repr
            .push(typar(TyparKind::Measure));
        // Generic over both a measure and a type — the type typar survives
        // into IL, so this one *is* IL-generic.
        let mut mixed = ext_test_val_arity(None, 0, None, &[1]);
        mixed.logical_name = "mixed".to_string();
        let mixed_repr = mixed
            .repr_info
            .as_mut()
            .expect("arity helper sets repr_info");
        mixed_repr.typar_repr.push(typar(TyparKind::Measure));
        mixed_repr.typar_repr.push(typar(TyparKind::Type));

        let module = make_entity(
            "M",
            PickledTyconRepr::NoRepr,
            module_with_vals(vec![0, 1, 2, 3, 4]),
        );
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(
            vec![root, module],
            vec![value, unit_fn, literal, measure_only, mixed],
            0,
        );

        let targets = collect_module_member_targets(&ccu).expect("collect");
        let vals = &targets[0].vals;

        // Value vs unit function: identical `il_arity`, split by group count.
        assert_eq!(vals[0].il_arity, Some(0));
        assert_eq!(vals[0].arg_group_count, Some(0));
        assert!(!vals[0].is_literal);
        assert_eq!(vals[1].il_arity, Some(0));
        assert_eq!(vals[1].arg_group_count, Some(1));

        // Literal: a value with a pickled constant.
        assert_eq!(vals[2].arg_group_count, Some(0));
        assert!(vals[2].is_literal);

        // Measure-only genericity is IL-erased; a type typar is not.
        assert!(!vals[3].is_generic);
        assert!(vals[4].is_generic);
    }

    /// FCS stores the *intrinsic* members of a type declared inside a module
    /// in the enclosing module's val list (on real FSharp.Core, the nested
    /// `StructBox`'s `.ctor`/`get_Value`/`get_Comparer` sit in
    /// `CompilerServices.RuntimeHelpers`' vals). Those compile onto the nested
    /// type's TypeDef, not the module's, so the index must exclude them —
    /// while keeping plain `let`s and `IsExtensionMember` augmentations, which
    /// do land on the module class. The kept entries' `val_index` still points
    /// at the original OSGN slots.
    #[test]
    fn module_member_index_excludes_nested_type_member_vals() {
        let ext = VAL_FLAGS_IS_EXTENSION_MEMBER;
        // A nested type's constructor and property getter: `member_info`
        // without the extension bit.
        let mut ctor = ext_test_val_arity(None, 0, Some(false), &[1]);
        ctor.logical_name = ".ctor".to_string();
        let mut getter = ext_test_val_arity(None, 0, Some(true), &[1]);
        getter.logical_name = "get_Value".to_string();
        // A plain module `let` and an instance augmentation, interleaved.
        let mut plain = ext_test_val_arity(None, 0, None, &[1]);
        plain.logical_name = "mkConcatSeq".to_string();
        let mut aug = ext_test_val_arity(Some("Counter.Tripled"), ext, Some(true), &[1, 0]);
        aug.logical_name = "Tripled".to_string();

        let module = make_entity(
            "M",
            PickledTyconRepr::NoRepr,
            module_with_vals(vec![0, 1, 2, 3]),
        );
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, module], vec![ctor, getter, plain, aug], 0);

        let targets = collect_module_member_targets(&ccu).expect("collect");
        assert_eq!(
            targets[0]
                .vals
                .iter()
                .map(|v| (v.val_index, v.logical_name.as_str()))
                .collect::<Vec<_>>(),
            [(2, "mkConcatSeq"), (3, "Tripled")]
        );
    }

    /// A val index past the OSGN table is a corrupt pickle: the index build
    /// fails loudly (D6.5) rather than skipping the val.
    #[test]
    fn module_member_index_rejects_out_of_range_val() {
        let module = make_entity("M", PickledTyconRepr::NoRepr, module_with_vals(vec![7]));
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, module], Vec::new(), 0);

        let err = collect_module_member_targets(&ccu).expect_err("out-of-range val index");
        assert!(
            matches!(
                err,
                ImportError::OsgnIndexOutOfRange {
                    kind: "val (module-member index)",
                    index: 7,
                    max: 0,
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    /// An all-extension same-`(name, arity)` overload set stays flagged (the
    /// group's facts are unanimous — the `TaskBuilder.MergeSources` shape),
    /// while a different-arity non-extension sharing the compiled name lands
    /// in its own claim group and stays unflagged.
    #[test]
    fn member_list_flags_unanimous_extension_overload_sets() {
        let ext = VAL_FLAGS_IS_EXTENSION_MEMBER;
        let entity = run_member_projection(
            vec![
                ext_test_val_arity(Some("Builder.MergeSources"), ext, Some(true), &[1, 1, 1]),
                ext_test_val_arity(Some("Builder.MergeSources"), ext, Some(true), &[1, 1, 1]),
                ext_test_val_arity(Some("Builder.MergeSources"), 0, None, &[1, 1]),
            ],
            vec![
                make_ecma_method_arity("Builder.MergeSources", 3),
                make_ecma_method_arity("Builder.MergeSources", 3),
                make_ecma_method_arity("Builder.MergeSources", 2),
            ],
        );
        let flags: Vec<(usize, bool)> = entity
            .members
            .iter()
            .map(|m| match m {
                Member::Method(m) => (m.signature.parameters.len(), m.is_extension_method),
                _ => panic!("module members are methods"),
            })
            .collect();
        assert_eq!(
            flags,
            [(3, true), (3, true), (2, false)],
            "the unanimous arity-3 extension group stays flagged; the arity-2 \
             plain let is a different claim group"
        );
        assert!(entity.skipped_members.is_empty());
    }

    /// The cross-module collision the per-module overlay must resolve: module
    /// `A` genuinely augments `Counter` (its `Tripled` val is an extension
    /// member, IL-mangled `Counter.Tripled`), while module `B` has a plain
    /// `[<CompiledName("Counter.Tripled")>] let` — the *same* compiled IL name
    /// in a different TypeDef. A bare-name match would flag both; scoping the
    /// pickle verdict to the declaring module flags only `A`'s.
    #[test]
    fn member_list_scopes_extension_flag_to_declaring_module() {
        let ext = VAL_FLAGS_IS_EXTENSION_MEMBER;
        // tycons: root(0) → namespace NS(1) → module A(2, val 0), module B(3, val 1)
        let module_a = make_entity("A", PickledTyconRepr::NoRepr, module_with_vals(vec![0]));
        let module_b = make_entity("B", PickledTyconRepr::NoRepr, module_with_vals(vec![1]));
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2, 3];
        let ns = make_entity("NS", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(
            vec![root, ns, module_a, module_b],
            vec![
                ext_test_val(Some("Counter.Tripled"), ext, Some(true)), // A: genuine extension
                ext_test_val(Some("Counter.Tripled"), 0, None),         // B: plain dotted-name let
            ],
            0,
        );

        let mut entities = vec![
            {
                let mut e = make_ecma_entity(vec!["NS"], "A", EntityKind::Module);
                e.members = vec![make_ecma_method("Counter.Tripled")];
                e
            },
            {
                let mut e = make_ecma_entity(vec!["NS"], "B", EntityKind::Module);
                e.members = vec![make_ecma_method("Counter.Tripled")];
                e
            },
        ];

        apply_module_member_projection(&mut entities, &ccu).expect("member projection");

        assert!(
            is_extension(&entities[0], "Counter.Tripled"),
            "module A's genuine augmentation must be flagged"
        );
        assert!(
            !is_extension(&entities[1], "Counter.Tripled"),
            "module B's plain let with the colliding compiled name must NOT be flagged"
        );
    }

    /// Builds a single-module CCU whose `Extensions` module declares two vals
    /// sharing the compiled name `Counter.Tripled` (val 0: instance extension;
    /// val 1: plain `let`), with the given IL arg-group shapes, plus an ECMA
    /// `Extensions` module carrying two `Counter.Tripled` methods of the given
    /// arities. Returns the entities after the member-list rebuild has run.
    /// NB the rebuilt member order is *val* order, so `members[0]` pairs with
    /// the extension val's claim and `members[1]` with the plain let's.
    fn run_in_module_collision(
        ext_arg_groups: &[usize],
        let_arg_groups: &[usize],
        ecma_arities: [usize; 2],
    ) -> Vec<Entity> {
        let ext = VAL_FLAGS_IS_EXTENSION_MEMBER;
        // tycons: root(0) → namespace NS(1) → module Extensions(2, vals 0,1)
        let module_ext = make_entity(
            "Extensions",
            PickledTyconRepr::NoRepr,
            module_with_vals(vec![0, 1]),
        );
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2];
        let ns = make_entity("NS", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(
            vec![root, ns, module_ext],
            vec![
                ext_test_val_arity(Some("Counter.Tripled"), ext, Some(true), ext_arg_groups),
                ext_test_val_arity(Some("Counter.Tripled"), 0, None, let_arg_groups),
            ],
            0,
        );

        let mut entities = vec![{
            let mut e = make_ecma_entity(vec!["NS"], "Extensions", EntityKind::Module);
            e.members = vec![
                make_ecma_method_arity("Counter.Tripled", ecma_arities[0]),
                make_ecma_method_arity("Counter.Tripled", ecma_arities[1]),
            ];
            e
        }];
        apply_module_member_projection(&mut entities, &ccu).expect("member projection");
        entities
    }

    fn method_is_extension(member: &Member) -> bool {
        matches!(member, Member::Method(mm) if mm.is_extension_method)
    }

    /// The reachable in-module collision: one `Extensions` module declares both
    /// the `Counter.Tripled` augmentation (compiled `Counter.Tripled`, IL arity
    /// 1 — the re-prepended receiver) and a plain
    /// `[<CompiledName("Counter.Tripled")>] let g a b` (IL arity 2). fsc emits
    /// both as `Counter.Tripled` MethodDefs on the module class. A per-name
    /// match flags both; arity disambiguation flags only the augmentation.
    #[test]
    fn member_list_disambiguates_in_module_collision_by_arity() {
        // ext: receiver group + unit group → IL arity 1; let: two curried args
        // → IL arity 2. ECMA methods at arity 1 and 2.
        let entities = run_in_module_collision(&[1, 0], &[1, 1], [1, 2]);
        assert!(
            method_is_extension(&entities[0].members[0]),
            "the arity-1 augmentation must be flagged"
        );
        assert!(
            !method_is_extension(&entities[0].members[1]),
            "the arity-2 plain let sharing the compiled name must NOT be flagged"
        );
    }

    /// The same-arity residual: the augmentation (`member this.M (x: T)`, IL
    /// arity 2 — receiver + one arg) and a plain `let g (a: U) (b: V)` (IL arity
    /// 2) collide on both name *and* arity. Arity cannot break the tie, so the
    /// overlay under-flags both rather than over-flag — declining to assert an
    /// annotation it cannot attribute to one val. Closing this needs
    /// signature-level matching (see `docs/fcs-divergences.md`).
    #[test]
    fn member_list_underflags_same_arity_in_module_collision() {
        // ext: receiver group + one arg group → IL arity 2; let: two curried
        // args → IL arity 2. Both ECMA methods at arity 2.
        let entities = run_in_module_collision(&[1, 1], &[1, 1], [2, 2]);
        assert!(
            !method_is_extension(&entities[0].members[0]),
            "same-arity collision under-flags: neither method is flagged"
        );
        assert!(
            !method_is_extension(&entities[0].members[1]),
            "same-arity collision under-flags: neither method is flagged"
        );
    }

    #[test]
    fn entity_source_name_cases() {
        // Module-with-suffix: pickled `logical_name` keeps the `"Module"`
        // suffix; the F# source name strips it.
        let suffixed = make_entity(
            "SuffixedModule",
            measure_object_model_repr(),
            module_suffix_modul_typ(vec![]),
        );
        assert_eq!(entity_source_name(&suffixed).as_deref(), Some("Suffixed"));

        // `[<CompiledName("Foo")>] type Bar`: compiled (CLR) name "Foo",
        // logical (source) name "Bar".
        let mut renamed = make_entity("Bar", measure_object_model_repr(), module_modul_typ(vec![]));
        renamed.compiled_name = Some("Foo".to_string());
        assert_eq!(entity_source_name(&renamed).as_deref(), Some("Bar"));

        // A *generic* renamed type: the pickled `logical_name` keeps the CLR
        // arity suffix (`Choice`2`), which the source name drops — as in
        // FSharp.Core's `[<CompiledName("FSharpChoice`2")>] type Choice<…>`.
        let mut generic = make_entity(
            "Choice`2",
            measure_object_model_repr(),
            module_modul_typ(vec![]),
        );
        generic.compiled_name = Some("FSharpChoice`2".to_string());
        assert_eq!(entity_source_name(&generic).as_deref(), Some("Choice"));

        // Plain module / type: IL name already is the source name.
        let plain = make_entity(
            "Hello",
            measure_object_model_repr(),
            module_modul_typ(vec![]),
        );
        assert_eq!(entity_source_name(&plain), None);

        // A namespace fragment is never a source-name target.
        let ns = make_entity("N", measure_object_model_repr(), empty_modul_typ());
        assert_eq!(entity_source_name(&ns), None);
    }

    /// Member source names ride the claim: a compiled-name overload collision
    /// (`sprintf`/`ksprintf` ⇒ `PrintFormatToStringThen`) is broken by the
    /// per-arity claim groups, and a collision arity cannot break (two
    /// *different* logical names renamed to the same compiled name at the
    /// same arity) under-sets rather than guesses.
    #[test]
    fn member_list_assigns_source_names_by_claim_group() {
        let mut sprintf = ext_test_val_arity(Some("PrintFormatToStringThen"), 0, None, &[1]);
        sprintf.logical_name = "sprintf".to_string();
        let mut ksprintf = ext_test_val_arity(Some("PrintFormatToStringThen"), 0, None, &[1, 1]);
        ksprintf.logical_name = "ksprintf".to_string();
        let entity = run_member_projection(
            vec![sprintf, ksprintf],
            vec![
                make_ecma_method_arity("PrintFormatToStringThen", 1),
                make_ecma_method_arity("PrintFormatToStringThen", 2),
            ],
        );
        let names: Vec<(usize, Option<&str>)> = entity
            .members
            .iter()
            .map(|m| match m {
                Member::Method(m) => (m.signature.parameters.len(), m.source_name.as_deref()),
                _ => panic!("module members are methods"),
            })
            .collect();
        assert_eq!(names, [(1, Some("sprintf")), (2, Some("ksprintf"))]);

        // Same-arity rename collision with *differing* logical names: the
        // group's source fact is conflicted, so both members under-set.
        let mut a = ext_test_val_arity(Some("Shared"), 0, None, &[1]);
        a.logical_name = "first".to_string();
        let mut b = ext_test_val_arity(Some("Shared"), 0, None, &[1]);
        b.logical_name = "second".to_string();
        let entity = run_member_projection(
            vec![a, b],
            vec![
                make_ecma_method_arity("Shared", 1),
                make_ecma_method_arity("Shared", 1),
            ],
        );
        for m in &entity.members {
            let Member::Method(m) = m else {
                panic!("module members are methods")
            };
            assert_eq!(
                m.source_name, None,
                "conflicted same-arity rename group must under-set"
            );
        }
    }

    /// Definition ranges ride the claim like source names: preferred from the
    /// pickled pair's `DefinitionRange` component (`other_range` — the
    /// implementation range, not the possibly-`.fsi` `val_range`), stamped on
    /// a unanimous claim group, and under-set when a same-`(name, shape)`
    /// group's vals disagree (which val claimed which MethodDef is then
    /// unprovable — a wrong range would navigate to a sibling overload).
    #[test]
    fn member_list_stamps_definition_ranges_by_claim_group() {
        let prange = |file: u32, line: u32| PickledRange {
            file,
            start: PickledPos { line, column: 4 },
            end: PickledPos { line, column: 10 },
        };
        // A value binding: `val_range` names the `.fsi`, `other_range` the `.fs`.
        let mut value = ext_test_val_arity(None, 0, None, &[]);
        value.logical_name = "answer".to_string();
        value.range = Some(prange(1, 20));
        value.other_range = Some(prange(0, 42));
        // Two same-name same-arity vals with different ranges: conflicted group.
        let mut a = ext_test_val_arity(Some("Shared"), 0, None, &[1]);
        a.logical_name = "first".to_string();
        a.range = Some(prange(0, 1));
        a.other_range = Some(prange(0, 1));
        let mut b = ext_test_val_arity(Some("Shared"), 0, None, &[1]);
        b.logical_name = "second".to_string();
        b.range = Some(prange(0, 2));
        b.other_range = Some(prange(0, 2));

        let val_indices: Vec<u32> = vec![0, 1, 2];
        let module = make_entity("M", PickledTyconRepr::NoRepr, module_with_vals(val_indices));
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2];
        let ns = make_entity("NS", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let mut ccu = make_ccu(vec![root, ns, module], vec![value, a, b], 0);
        ccu.header.strings = vec!["/src/Impl.fs".to_string(), "/src/Sig.fsi".to_string()];

        let mut entities = vec![{
            let mut e = make_ecma_entity(vec!["NS"], "M", EntityKind::Module);
            e.members = vec![
                make_ecma_module_value("answer"),
                make_ecma_method_arity("Shared", 1),
                make_ecma_method_arity("Shared", 1),
            ];
            e
        }];
        apply_module_member_projection(&mut entities, &ccu).expect("member projection");
        let entity = entities.into_iter().next().expect("one entity");

        let method = |name: &str, params: usize| {
            entity
                .members
                .iter()
                .find_map(|m| match m {
                    Member::Method(m)
                        if m.name == name && m.signature.parameters.len() == params =>
                    {
                        Some(m)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("method {name}/{params}"))
        };
        let range = method("answer", 0)
            .definition_range
            .as_deref()
            .expect("a singleton value group stamps its range");
        assert_eq!(
            (range.file.as_str(), range.start_line, range.start_column),
            ("/src/Impl.fs", 42, 4),
            "the DefinitionRange (implementation) component wins over val_range"
        );
        assert_eq!((range.end_line, range.end_column), (42, 10));
        assert_eq!(
            method("Shared", 1).definition_range,
            None,
            "a conflicted same-shape group must under-set its ranges"
        );
    }

    /// A minimal rebranded module-value member: a zero-parameter method
    /// carrying [`ModuleValue`] (what `project_fsharp_members` emits for a
    /// module `let` value's static property).
    fn make_ecma_module_value(name: &str) -> Member {
        let mut m = make_ecma_method(name);
        if let Member::Method(mm) = &mut m {
            mm.module_value = Some(crate::model::ModuleValue { is_mutable: false });
        }
        m
    }

    /// The claim shapes: a *value* val (zero argument groups) claims only the
    /// `module_value`-marked member, never the identically-named zero-arg
    /// *function* — and vice versa — so `let answer = 42` and `let f () = 1`
    /// can coexist under colliding compiled names without cross-claiming.
    /// Source names ride the value claim too (the `async` ⇒
    /// `DefaultAsyncBuilder` shape).
    #[test]
    fn member_list_matches_values_and_functions_by_shape() {
        let mut value = ext_test_val_arity(Some("Shared"), 0, None, &[]);
        value.logical_name = "answer".to_string();
        let mut unit_fn = ext_test_val_arity(Some("Shared"), 0, None, &[0]);
        unit_fn.logical_name = "f".to_string();
        let entity = run_member_projection(
            vec![value, unit_fn],
            vec![
                // Deliberately listed function-first: the value val must skip
                // past it to the module_value member.
                make_ecma_method_arity("Shared", 0),
                make_ecma_module_value("Shared"),
            ],
        );
        let shapes: Vec<(bool, Option<&str>)> = entity
            .members
            .iter()
            .map(|m| match m {
                Member::Method(m) => (m.module_value.is_some(), m.source_name.as_deref()),
                _ => panic!("module members are methods"),
            })
            .collect();
        // Rebuilt in val order: the value claim first, then the function.
        assert_eq!(shapes, [(true, Some("answer")), (false, Some("f"))]);
        assert!(entity.skipped_members.is_empty());
    }

    /// A `[<Literal>]` module val claims the static literal field fsc emits for it, so
    /// the projected member list carries the constant. FCS brings it into bare scope
    /// (fsi: `open M` then `LitVal` compiles), and the old silent elision left a bare
    /// name no consumer could see — the hole the Slice-A review found.
    #[test]
    fn member_list_claims_a_literal_val_to_its_il_field() {
        let mut literal = ext_test_val_arity(None, 0, None, &[]);
        literal.logical_name = "MaxValue".to_string();
        literal.literal_value = Some(PickledConst::Int32(100));
        let entity =
            run_member_projection(vec![literal], vec![make_ecma_literal_field("MaxValue")]);
        assert!(
            entity.skipped_members.is_empty(),
            "the literal is claimable: {:?}",
            entity.skipped_members
        );
        match entity.members.as_slice() {
            [Member::Field(f)] => {
                assert_eq!(f.name, "MaxValue");
                assert!(f.is_literal, "claimed the literal field");
            }
            other => panic!("expected the claimed literal field, got {other:?}"),
        }
    }

    /// The mismatch policy, pinned (plan §3 Slice C risk): a val with no claimable
    /// IL member, and an IL member no val claims, are both recorded on
    /// `skipped_members` — loud, bounded uncertainty. A `[<Literal>]` val is **not**
    /// exempt: it claims the static literal field fsc emits, and a literal whose
    /// field is absent (or `[<CompiledName>]`-renamed, which `Field` cannot carry) is
    /// recorded like any other unclaimable val. It used to be elided silently, which
    /// left an *invisible* bare name — FCS resolves `open M; MaxValue` (fsi-verified),
    /// so a consumer could not even know to be conservative about it (Slice-A review
    /// of `docs/assembly-module-open-plan.md`).
    #[test]
    fn member_list_records_unmatched_vals_and_members() {
        let mut missing = ext_test_val_arity(Some("NoSuchMethod"), 0, None, &[1]);
        missing.logical_name = "missing".to_string();
        let mut literal = ext_test_val_arity(None, 0, None, &[]);
        literal.logical_name = "MaxValue".to_string();
        literal.literal_value = Some(PickledConst::Int32(100));
        let entity = run_member_projection(
            vec![missing, literal],
            // An IL-only artefact nothing claims (the witness-twin shape).
            vec![make_ecma_method_arity("ToSingle$W", 2)],
        );
        assert!(
            entity.members.is_empty(),
            "nothing claimable: {:?}",
            entity.members.len()
        );
        let skips: Vec<(&str, &str)> = entity
            .skipped_members
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(
            skips.len(),
            3,
            "one per direction, plus the literal whose IL field is absent: {skips:?}"
        );
        assert!(
            skips[0].0 == "NoSuchMethod" && skips[0].1.contains("no matching projected IL member"),
            "{skips:?}"
        );
        assert!(
            skips[1].0 == "MaxValue" && skips[1].1.contains("no claimable IL literal field"),
            "{skips:?}"
        );
        assert!(
            skips[2].0 == "ToSingle$W" && skips[2].1.contains("no pickled module val"),
            "{skips:?}"
        );
    }

    /// The review-caught zero-group *generic* val shape
    /// (`typeof<'T>`/`sizeof<'T>`/`Unchecked.defaultof<'T>`): a CLR property
    /// cannot be generic, so fsc emits a generic MethodDef with zero
    /// parameters — the claim must go down the `Function(0)` path, not
    /// `Value`, while a plain zero-group val still claims the
    /// `module_value`-marked property method.
    #[test]
    fn member_list_claims_generic_zero_group_vals_as_methods() {
        let mut type_of = ext_test_val_arity(Some("TypeOf"), 0, None, &[]);
        type_of.logical_name = "typeof".to_string();
        type_of
            .repr_info
            .as_mut()
            .expect("arity helper sets repr_info")
            .typar_repr
            .push(PickledTyparReprInfo {
                ident: PickledIdent {
                    name: "T".to_string(),
                    range: dummy_range(),
                },
                kind: TyparKind::Type,
            });
        let mut plain_value = ext_test_val_arity(None, 0, None, &[]);
        plain_value.logical_name = "answer".to_string();
        let entity = run_member_projection(
            vec![type_of, plain_value],
            vec![
                make_ecma_method_arity("TypeOf", 0),
                make_ecma_module_value("answer"),
            ],
        );
        let shapes: Vec<(&str, bool)> = entity
            .members
            .iter()
            .map(|m| match m {
                Member::Method(m) => (m.name.as_str(), m.module_value.is_some()),
                _ => panic!("module members are methods"),
            })
            .collect();
        assert_eq!(shapes, [("TypeOf", false), ("answer", true)]);
        assert_eq!(
            entity.members.len(),
            2,
            "both zero-group vals claim: {:?}",
            entity.skipped_members
        );
        assert!(entity.skipped_members.is_empty());
    }

    /// The review-caught leftover policy: a *non-public* projected member no
    /// val claims is fsc's own machinery (a lambda-lifted closure
    /// `concatArray@29`, a private helper) — the signature pickle never
    /// describes it, so it is *retained* silently, exactly as the
    /// IL-driven list kept it. Only a *public* unclaimed member is recorded
    /// as a skip (covered by `member_list_records_unmatched_vals_and_members`).
    #[test]
    fn member_list_retains_non_public_leftovers() {
        let mut lifted = make_ecma_method_arity("concatArray@29", 1);
        if let Member::Method(m) = &mut lifted {
            m.access = Access::Internal;
        }
        let mut private_helper = make_ecma_method_arity("check@188", 2);
        if let Member::Method(m) = &mut private_helper {
            m.access = Access::Private;
        }
        let mut public_let = ext_test_val_arity(None, 0, None, &[1]);
        public_let.logical_name = "concat".to_string();
        let entity = run_member_projection(
            vec![public_let],
            vec![lifted, make_ecma_method_arity("concat", 1), private_helper],
        );
        let names: Vec<&str> = entity
            .members
            .iter()
            .map(|m| match m {
                Member::Method(m) => m.name.as_str(),
                _ => panic!("module members are methods"),
            })
            .collect();
        // Claimed members first (val order), retained leftovers after.
        assert_eq!(names, ["concat", "concatArray@29", "check@188"]);
        assert!(
            entity.skipped_members.is_empty(),
            "non-public leftovers are expected, not skips: {:?}",
            entity.skipped_members
        );
    }

    /// The `.fsi`-hidden-helper collision the review caught: a signature file
    /// exports `Shared : string -> string` while the implementation also has
    /// a *private* `[<CompiledName("Shared")>]` helper of the same arity —
    /// the helper is absent from the signature pickle, so name/shape/arity
    /// alone cannot tell the two IL methods apart. The val's pickled
    /// accessibility class must break the tie: the public val claims the
    /// *public* MethodDef (even listed second), and the hidden helper is
    /// retained as a non-public leftover rather than the exported method
    /// being discarded as an unclaimed public one.
    #[test]
    fn member_list_prefers_accessibility_matching_members() {
        let mut hidden = make_ecma_method_arity("Shared", 1);
        if let Member::Method(m) = &mut hidden {
            m.access = Access::Internal;
        }
        let mut exported = ext_test_val_arity(Some("Shared"), 0, None, &[1]);
        exported.logical_name = "shared".to_string();
        let entity = run_member_projection(
            vec![exported],
            vec![hidden, make_ecma_method_arity("Shared", 1)],
        );
        let claimed: Vec<(Access, Option<&str>)> = entity
            .members
            .iter()
            .map(|m| match m {
                Member::Method(m) => (m.access, m.source_name.as_deref()),
                _ => panic!("module members are methods"),
            })
            .collect();
        // Claimed public member first (with the val's source name), retained
        // internal leftover after (no facts stamped).
        assert_eq!(
            claimed,
            [(Access::Public, Some("shared")), (Access::Internal, None)]
        );
        assert!(entity.skipped_members.is_empty());
    }

    /// The kind gate: a *class* whose FQN the pickle also describes keeps its
    /// IL-projected members untouched — the val-driven list is for modules
    /// only (a class's member vals live in `tcaug.adhoc`, not
    /// `module_type.vals`; plan §2).
    #[test]
    fn member_projection_leaves_non_module_kinds_alone() {
        let class = make_entity("C", PickledTyconRepr::NoRepr, module_modul_typ(Vec::new()));
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, class], Vec::new(), 0);

        let mut entities = vec![{
            let mut e = make_ecma_entity(vec![], "C", EntityKind::Class);
            e.members = vec![make_ecma_method("KeepMe")];
            e
        }];
        apply_module_member_projection(&mut entities, &ccu).expect("member projection");
        assert_eq!(entities[0].members.len(), 1, "class members untouched");
        assert!(entities[0].skipped_members.is_empty());
    }

    #[test]
    fn source_name_overlay_matches_generic_renamed_entity() {
        // Pickle: root → namespace "N" → a generic renamed type whose pickled
        // names carry the CLR arity suffix (`Choice`2` / `FSharpChoice`2`), as
        // in FSharp.Core. The overlay must still locate the ECMA entity (keyed
        // by the *arity-stripped* CLR name `FSharpChoice`) and set its source
        // name to the arity-stripped display name `Choice`.
        let mut generic = make_entity(
            "Choice`2",
            measure_object_model_repr(),
            module_modul_typ(vec![]),
        );
        generic.compiled_name = Some("FSharpChoice`2".to_string());
        let mut ns_modul = empty_modul_typ(); // IsType::Namespace
        ns_modul.entities = vec![2];
        let ns = make_entity("N", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Asm", PickledTyconRepr::NoRepr, root_modul);
        let ccu = PickledCcu {
            header: PickledHeader {
                ccu_refs: Vec::new(),
                ntycons: 3,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, ns, generic],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        };
        // The projector stores `Entity::name` arity-stripped.
        let mut owned = vec![make_ecma_entity(
            vec!["N"],
            "FSharpChoice",
            EntityKind::Class,
        )];
        apply_entity_overlay(&mut owned, &ccu).unwrap();
        assert_eq!(owned[0].source_name.as_deref(), Some("Choice"));
    }

    // -----------------------------------------------------------------
    // Cycle guard (`walk_entity_tree`)
    // -----------------------------------------------------------------
    //
    // A valid FCS entity graph is a tree, so these cyclic tables can only
    // come from a corrupt or crafted pickle. Without the `path` guard each
    // walk would recurse unboundedly and abort the process on stack
    // overflow (the overlays run on the caller's normal stack). The guard
    // turns that into a recoverable `PickleEntityCycle` — which is what
    // these tests pin, exercised through all three public overlay entry
    // points (each now funnels through the shared `walk_entity_tree`).

    /// A one-element self-loop: the sole non-root entity lists its own stamp
    /// as a child. Caught the first time the walk descends back into it.
    #[test]
    fn measure_overlay_rejects_self_loop() {
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![1]; // stamp 1 is this very entity
        let ns = make_entity("NS", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, ns], Vec::new(), 0);

        let err = merge_measure_entities(&mut [], &ccu).unwrap_err();
        match err {
            ImportError::PickleEntityCycle { stamp } => assert_eq!(stamp, 1),
            other => panic!("expected PickleEntityCycle, got {other:?}"),
        }
    }

    /// A two-entity cycle 1 → 2 → 1. The guard trips on the back-edge to
    /// stamp 1, which is still on the descent path via stamp 2.
    #[test]
    fn member_projection_rejects_two_cycle() {
        let mut a_modul = empty_modul_typ();
        a_modul.entities = vec![2];
        let a = make_entity("A", PickledTyconRepr::NoRepr, a_modul);
        let mut b_modul = empty_modul_typ();
        b_modul.entities = vec![1]; // back to A
        let b = make_entity("B", PickledTyconRepr::NoRepr, b_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, a, b], Vec::new(), 0);

        let err = apply_module_member_projection(&mut [], &ccu).unwrap_err();
        match err {
            ImportError::PickleEntityCycle { stamp } => assert_eq!(stamp, 1),
            other => panic!("expected PickleEntityCycle, got {other:?}"),
        }
    }

    /// A back-edge to the synthetic root (stamp 0): a descendant lists the
    /// root's stamp as a child. The root is on every descent path, so the
    /// guard catches it too.
    #[test]
    fn source_name_overlay_rejects_back_edge_to_root() {
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![0]; // back to the root
        let ns = make_entity("NS", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, ns], Vec::new(), 0);

        let err = apply_entity_overlay(&mut [], &ccu).unwrap_err();
        match err {
            ImportError::PickleEntityCycle { stamp } => assert_eq!(stamp, 0),
            other => panic!("expected PickleEntityCycle, got {other:?}"),
        }
    }

    /// A diamond (a stamp reachable by two *disjoint* paths, no back-edge) is
    /// not a cycle: the `path` set only holds the current descent, so the
    /// second visit — after the first has been popped — is allowed. This pins
    /// that the guard is a path set, not a global visited set, and so cannot
    /// reject the (tree-shaped) shapes FCS actually emits.
    #[test]
    fn shared_child_on_disjoint_paths_is_not_a_cycle() {
        // root(0) → A(1) → shared(3); root(0) → B(2) → shared(3).
        let mut a_modul = empty_modul_typ();
        a_modul.entities = vec![3];
        let a = make_entity("A", PickledTyconRepr::NoRepr, a_modul);
        let mut b_modul = empty_modul_typ();
        b_modul.entities = vec![3];
        let b = make_entity("B", PickledTyconRepr::NoRepr, b_modul);
        let shared = make_entity("Shared", PickledTyconRepr::NoRepr, empty_modul_typ());
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1, 2];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, a, b, shared], Vec::new(), 0);

        // No cycle → the walk completes; with no measures nothing is recorded.
        merge_measure_entities(&mut [], &ccu).expect("diamond is acyclic");
    }

    // -----------------------------------------------------------------------
    // Abbreviation shadow markers
    // -----------------------------------------------------------------------

    /// Any pickled type will do as an abbreviation target — the marker
    /// synthesis only tests `type_abbrev.is_some()`, never the payload.
    fn some_abbrev_target() -> Option<PickledType> {
        Some(PickledType::Var {
            typar_index: 0,
            nullness: Nullness::Ambivalent,
        })
    }

    fn make_abbreviation(logical_name: &str) -> PickledEntity {
        let mut e = make_entity(logical_name, PickledTyconRepr::NoRepr, empty_modul_typ());
        e.type_abbrev = some_abbrev_target();
        e
    }

    /// Pickle: root(CCU "Test") → namespace "Lib" containing an abbreviation
    /// `IntId`, a module `Auto` with a nested abbreviation `Nested`, a
    /// `private` abbreviation `Hidden`, a measure abbreviation `msq`, and an
    /// abbreviation `Existing` that also has an ECMA row.
    fn ccu_with_abbreviations() -> PickledCcu {
        let int_id = make_abbreviation("IntId"); // index 2
        let nested = make_abbreviation("Nested"); // index 3
        let auto = make_entity("Auto", PickledTyconRepr::NoRepr, module_modul_typ(vec![3])); // 4
        let mut hidden = make_abbreviation("Hidden"); // 5
        hidden.access = vec![PickledCPath {
            scope: PickledILScopeRef::Local,
            path: vec![("Lib".to_string(), IsType::Namespace)],
        }];
        let mut msq = make_abbreviation("msq"); // 6
        msq.typar_kind = TyparKind::Measure;
        let existing = make_abbreviation("Existing"); // 7
        let mut lib_modul = empty_modul_typ();
        lib_modul.entities = vec![2, 4, 5, 6, 7];
        let lib = make_entity("Lib", PickledTyconRepr::NoRepr, lib_modul); // 1
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul); // 0
        make_ccu(
            vec![root, lib, int_id, nested, auto, hidden, msq, existing],
            Vec::new(),
            0,
        )
    }

    /// The matching ECMA tree: the module `Auto` and the type `Existing`
    /// have TypeDef rows; none of the abbreviations do.
    fn abbreviation_ecma_tree() -> Vec<Entity> {
        vec![
            make_ecma_entity(vec!["Lib"], "Auto", EntityKind::Module),
            make_ecma_entity(vec!["Lib"], "Existing", EntityKind::Class),
        ]
    }

    #[test]
    fn synthesises_root_and_nested_abbreviation_markers() {
        let pickled = ccu_with_abbreviations();
        let mut entities = abbreviation_ecma_tree();
        apply_abbreviation_markers(&mut entities, &pickled, &dummy_assembly())
            .expect("marker synthesis");

        let int_id = entities
            .iter()
            .find(|e| e.name == "IntId")
            .expect("namespace-level abbreviation gets a root marker");
        assert_eq!(int_id.namespace, vec!["Lib".to_string()]);
        assert_eq!(int_id.kind, EntityKind::Abbreviation);
        assert_eq!(int_id.access, Access::Public);
        assert!(int_id.members.is_empty() && int_id.nested_types.is_empty());

        let auto = entities.iter().find(|e| e.name == "Auto").expect("module");
        let nested = auto
            .nested_types
            .iter()
            .find(|e| e.name == "Nested")
            .expect("module-scoped abbreviation gets a nested marker");
        assert_eq!(nested.kind, EntityKind::Abbreviation);
        assert!(
            nested.namespace.is_empty(),
            "nested entities carry an empty namespace (path lives on the outermost type)"
        );
    }

    #[test]
    fn skips_private_measure_and_already_present_abbreviations() {
        let pickled = ccu_with_abbreviations();
        let mut entities = abbreviation_ecma_tree();
        apply_abbreviation_markers(&mut entities, &pickled, &dummy_assembly())
            .expect("marker synthesis");

        assert!(
            !entities.iter().any(|e| e.name == "Hidden"),
            "a non-public abbreviation is not nameable cross-assembly — no marker"
        );
        assert!(
            !entities.iter().any(|e| e.name == "msq"),
            "a measure abbreviation is not a type-position name — no marker"
        );
        let existing: Vec<_> = entities.iter().filter(|e| e.name == "Existing").collect();
        assert_eq!(
            existing.len(),
            1,
            "an abbreviation with an ECMA row is not metadata-invisible — no duplicate"
        );
        assert_eq!(
            existing[0].kind,
            EntityKind::Class,
            "the authoritative ECMA row keeps its kind"
        );
    }

    #[test]
    fn fsharp_core_synthesises_no_markers() {
        let pickled = ccu_with_abbreviations();
        let mut entities = abbreviation_ecma_tree();
        let mut core = dummy_assembly();
        core.name = "FSharp.Core".to_string();
        apply_abbreviation_markers(&mut entities, &pickled, &core).expect("marker synthesis");
        assert_eq!(
            entities,
            abbreviation_ecma_tree(),
            "FSharp.Core's abbreviations are the primitive-alias semantics, never markers"
        );
    }

    #[test]
    fn generic_abbreviation_marker_carries_pickled_typar_names() {
        let mut pair = make_abbreviation("Pair");
        pair.typars = vec![0, 1];
        let mut lib_modul = empty_modul_typ();
        lib_modul.entities = vec![2];
        let lib = make_entity("Lib", PickledTyconRepr::NoRepr, lib_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let mut ccu = make_ccu(vec![root, lib, pair], Vec::new(), 0);
        ccu.tables.typars = ["a", "b"]
            .into_iter()
            .map(|name| PickledTyparSpecData {
                ident: PickledIdent {
                    name: name.to_string(),
                    range: dummy_range(),
                },
                attribs: Vec::new(),
                flags: 0,
                constraints: Vec::new(),
                xmldoc: PickledXmlDoc { lines: Vec::new() },
            })
            .collect();

        let mut entities = Vec::new();
        apply_abbreviation_markers(&mut entities, &ccu, &dummy_assembly())
            .expect("marker synthesis");
        let pair = entities.iter().find(|e| e.name == "Pair").expect("marker");
        let names: Vec<_> = pair
            .generic_parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["a", "b"],
            "arity-keyed lookups need the pickled typar list"
        );
    }

    #[test]
    fn arity_overloaded_abbreviation_still_gets_a_marker() {
        // `type Foo = { … }` (arity-0 TypeDef) and `type Foo<'T> = 'T list`
        // (arity-1 abbreviation) legally coexist: the ECMA row must suppress
        // only the SAME-ARITY marker (codex review, round 2).
        let mut generic = make_abbreviation("Foo");
        generic.typars = vec![0];
        let mut lib_modul = empty_modul_typ();
        lib_modul.entities = vec![2];
        let lib = make_entity("Lib", PickledTyconRepr::NoRepr, lib_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let mut ccu = make_ccu(vec![root, lib, generic], Vec::new(), 0);
        ccu.tables.typars = vec![PickledTyparSpecData {
            ident: PickledIdent {
                name: "T".to_string(),
                range: dummy_range(),
            },
            attribs: Vec::new(),
            flags: 0,
            constraints: Vec::new(),
            xmldoc: PickledXmlDoc { lines: Vec::new() },
        }];

        // The ECMA tree has the arity-0 record `Foo`.
        let mut entities = vec![make_ecma_entity(vec!["Lib"], "Foo", EntityKind::Record)];
        apply_abbreviation_markers(&mut entities, &ccu, &dummy_assembly())
            .expect("marker synthesis");
        let markers: Vec<_> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Abbreviation)
            .collect();
        assert_eq!(
            markers.len(),
            1,
            "the arity-1 abbreviation needs its marker"
        );
        assert_eq!(markers[0].generic_parameters.len(), 1);
        assert_eq!(
            entities
                .iter()
                .filter(|e| e.name == "Foo" && e.generic_parameters.is_empty())
                .count(),
            1,
            "the arity-0 TypeDef is not duplicated"
        );
    }

    #[test]
    fn compiled_name_sharing_abbreviations_each_get_a_marker() {
        // codex round 3: `[<CompiledName("X")>] type A = string` and
        // `[<CompiledName("X")>] type B = int` are distinct source types that
        // fsc accepts (abbreviations emit no TypeDefs, so the compiled names
        // never collide in metadata). The dedupe must key on the SOURCE
        // lookup name, or `B` loses its marker while FCS still binds `Lib.B`.
        let mut a = make_abbreviation("A");
        a.compiled_name = Some("X".to_string());
        let mut b = make_abbreviation("B");
        b.compiled_name = Some("X".to_string());
        let mut lib_modul = empty_modul_typ();
        lib_modul.entities = vec![2, 3];
        let lib = make_entity("Lib", PickledTyconRepr::NoRepr, lib_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, lib, a, b], Vec::new(), 0);

        let mut entities = Vec::new();
        apply_abbreviation_markers(&mut entities, &ccu, &dummy_assembly())
            .expect("marker synthesis");
        let mut source_names: Vec<_> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::Abbreviation)
            .map(|e| {
                e.source_name
                    .clone()
                    .expect("renamed markers carry source names")
            })
            .collect();
        source_names.sort();
        assert_eq!(
            source_names,
            vec!["A".to_string(), "B".to_string()],
            "both compiled-name-sharing abbreviations keep their own marker"
        );
    }

    #[test]
    fn module_companion_row_does_not_suppress_the_marker() {
        // codex round 4: the suffixed module companion's overlaid
        // `source_name` matches the abbreviation's lookup name, but a module
        // never occupies the type-position name — the marker must still be
        // synthesised.
        let abbrev = make_abbreviation("Companion");
        let mut lib_modul = empty_modul_typ();
        lib_modul.entities = vec![2];
        let lib = make_entity("Lib", PickledTyconRepr::NoRepr, lib_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(vec![root, lib, abbrev], Vec::new(), 0);

        let mut companion_module =
            make_ecma_entity(vec!["Lib"], "CompanionModule", EntityKind::Module);
        companion_module.source_name = Some("Companion".to_string());
        let mut entities = vec![companion_module];
        apply_abbreviation_markers(&mut entities, &ccu, &dummy_assembly())
            .expect("marker synthesis");
        assert!(
            entities
                .iter()
                .any(|e| e.kind == EntityKind::Abbreviation && e.name == "Companion"),
            "the module companion must not count as the abbreviation's ECMA row"
        );
    }

    #[test]
    fn declaration_order_overlay_restores_pickle_order() {
        // Pickle declares (in `Lib`): module First, then module Second — with
        // First nesting InnerA then InnerB. The ECMA tree arrives with both
        // levels reversed plus an unpickled compiler-generated root that must
        // keep its (relative) place at the end.
        let inner_a = make_entity("InnerA", PickledTyconRepr::NoRepr, module_modul_typ(vec![]));
        let inner_b = make_entity("InnerB", PickledTyconRepr::NoRepr, module_modul_typ(vec![]));
        let first = make_entity(
            "First",
            PickledTyconRepr::NoRepr,
            module_modul_typ(vec![2, 3]),
        );
        let second = make_entity("Second", PickledTyconRepr::NoRepr, module_modul_typ(vec![]));
        let mut lib_modul = empty_modul_typ();
        lib_modul.entities = vec![4, 5];
        let lib = make_entity("Lib", PickledTyconRepr::NoRepr, lib_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(
            vec![root, lib, inner_a, inner_b, first, second],
            Vec::new(),
            0,
        );

        let mut first_ecma = make_ecma_entity(vec!["Lib"], "First", EntityKind::Module);
        first_ecma
            .nested_types
            .push(make_ecma_entity(vec![], "InnerB", EntityKind::Module));
        first_ecma
            .nested_types
            .push(make_ecma_entity(vec![], "InnerA", EntityKind::Module));
        let mut entities = vec![
            make_ecma_entity(vec!["Lib"], "Second", EntityKind::Module),
            first_ecma,
            make_ecma_entity(vec![], "<StartupCode$Test>", EntityKind::Class),
        ];
        apply_declaration_order(&mut entities, &ccu).expect("declaration order overlay");

        let top: Vec<&str> = entities.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            top,
            vec!["First", "Second", "<StartupCode$Test>"],
            "top level takes pickle order; unpickled entities keep the tail"
        );
        let nested: Vec<&str> = entities[0]
            .nested_types
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(nested, vec!["InnerA", "InnerB"], "nested take pickle order");
    }

    /// A CCU whose `Ns` namespace holds a type `Teq` and a module `Helpers`,
    /// both pickled with `NoRepr`. `NoRepr` is the SIGNATURE shape a union gets
    /// when a signature file (or an inline `[<Sealed>]`) hides its
    /// representation from cross-assembly consumers — `type Teq<'a,'b>` in the
    /// `.fsi`, `type Teq<'a,'b> = private Teq of …` in the `.fs`. The signature
    /// pickle carries no union repr, yet the compiled class still bears
    /// `CompilationMapping(SumType)`, so the ECMA projector classifies it
    /// `EntityKind::Union`. A module pickles `NoRepr` too and carries the
    /// `IsModuleOrNamespace` flag (like the namespace and the root) — the merge
    /// must seal only the *type*. Slot 0 is the CCU wrapper, 1 the namespace,
    /// 2 `Teq`, 3 `Helpers`.
    fn ccu_with_hidden_union() -> PickledCcu {
        // A TYPE: `IsModuleOrNamespace` clear (`make_entity` defaults `flags` to 0).
        let teq = make_entity("Teq", PickledTyconRepr::NoRepr, empty_modul_typ());
        let mut helpers = make_entity(
            "Helpers",
            PickledTyconRepr::NoRepr,
            module_modul_typ(Vec::new()),
        );
        helpers.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2, 3];
        let mut ns = make_entity("Ns", PickledTyconRepr::NoRepr, ns_modul);
        ns.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let mut root = make_entity("Ns", PickledTyconRepr::NoRepr, root_modul);
        root.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        PickledCcu {
            header: PickledHeader {
                ccu_refs: vec![CcuRef {
                    name: "FSharp.Core".to_string(),
                }],
                ntycons: 4,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, ns, teq, helpers],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        }
    }

    #[test]
    fn a_signature_hidden_union_has_knowably_zero_accessible_cases() {
        // Regression: `type Teq<'a,'b>` abstract in a `.fsi`, union in the
        // `.fs`, pickles the SIGNATURE as `NoRepr`, so `apply_union_case_names`
        // never reaches it via the union-repr walk and it kept
        // `union_case_names = None` — read downstream (`assembly_env.rs`
        // `fold_tycon_tier`) as "unknowable hidden cases", which made `open`ing
        // the namespace defer every dotted head (e.g. `List.replicate` where a
        // file-local union case `List` forces the dotted-path branch). But a
        // representation hidden by the signature exposes ZERO accessible cases,
        // so the answer is a knowably-empty `Some(vec![])`, exactly like a
        // private representation's private-case filter. A same-shaped `NoRepr`
        // module (the `IsModuleOrNamespace` flag set) must stay untouched — it is
        // never even a candidate.
        let pickled = ccu_with_hidden_union();
        let mut entities = vec![
            make_ecma_entity(vec!["Ns"], "Teq", EntityKind::Union),
            make_ecma_entity(vec!["Ns"], "Helpers", EntityKind::Module),
        ];
        apply_union_case_names(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_case_names.as_deref(),
            Some(&[][..]),
            "a signature-hidden union has knowably zero accessible cases"
        );
        assert_eq!(
            entities[1].union_case_names, None,
            "a NoRepr module is not a union and must not be sealed"
        );
    }

    #[test]
    fn a_host_namespace_node_never_seals_a_foreign_union_at_its_fqn() {
        // codex's exact collision: the host declares `namespace Collision.U` (a
        // namespace — `NoRepr`, `IsModuleOrNamespace` set — in the host pickle),
        // while a linked dependency's `namespace Collision; type U = A | B` is a
        // copied foreign union in the ECMA tree at the same FQN (`Collision.U`,
        // arity 0), unknowable (`None`) because the host pickle does not describe
        // it. Were the host namespace node a candidate it would match that union
        // by `(namespace, name, arity)` and ECMA `Union` kind and falsely seal
        // its real cases empty. The `IsModuleOrNamespace` filter must exclude the
        // namespace node from `hidden_repr`, leaving the foreign union `None`.
        // (This is why the seal does not — and must not — trust `authoritative`:
        // a `--nointerfacedata` dependency's copied types bring no signature
        // resource, so the image can still read as authoritative here.)
        let mut ns_u = empty_modul_typ();
        ns_u.is_type = IsType::Namespace;
        let mut u = make_entity("U", PickledTyconRepr::NoRepr, ns_u);
        u.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let mut collision = empty_modul_typ();
        collision.entities = vec![2];
        let mut collision = make_entity("Collision", PickledTyconRepr::NoRepr, collision);
        collision.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Host", PickledTyconRepr::NoRepr, root_modul);
        let pickled = PickledCcu {
            header: PickledHeader {
                ccu_refs: Vec::new(),
                ntycons: 3,
                ntypars: 0,
                nvals: 0,
                nanoninfos: 0,
                strings: Vec::new(),
                pubpaths: Vec::new(),
                nlerefs: Vec::new(),
                simpletys: Vec::new(),
                phase1_bytes: Vec::new(),
            },
            root_entity: 0,
            compile_time_working_dir: String::new(),
            uses_quotations: false,
            tables: PickledOsgnTables {
                tycons: vec![root, collision, u],
                typars: Vec::new(),
                vals: Vec::new(),
            },
        };
        // The copied foreign union at `Collision.U` — unknowable, `None`.
        let mut entities = vec![make_ecma_entity(vec!["Collision"], "U", EntityKind::Union)];
        apply_union_case_names(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_case_names, None,
            "a host namespace node must never seal a foreign union at its FQN"
        );
    }

    #[test]
    fn an_exception_never_seals_a_foreign_union_at_its_fqn() {
        // codex round 3: an `exception U = …` alias is a non-module `NoRepr`
        // entity with no `type_abbrev` — it would satisfy the opaque-type filter
        // — yet it emits no TypeDef of its own, so in a static-linked image its
        // FQN can be occupied by a copied foreign union. The `exn_repr` exclusion
        // must keep it out of `hidden_repr`, leaving that foreign union `None`.
        // (`Fresh` here stands in for any non-`None` `exn_repr`; the exclusion is
        // identical for the `Abbrev` alias that is the realistic collision.)
        let mut u = make_entity("U", PickledTyconRepr::NoRepr, empty_modul_typ());
        u.exn_repr = PickledExnRepr::Fresh(Vec::new());
        let mut collision = empty_modul_typ();
        collision.entities = vec![2];
        let mut collision = make_entity("Collision", PickledTyconRepr::NoRepr, collision);
        collision.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Host", PickledTyconRepr::NoRepr, root_modul);
        let pickled = make_ccu(vec![root, collision, u], Vec::new(), 0);
        let mut entities = vec![make_ecma_entity(vec!["Collision"], "U", EntityKind::Union)];
        apply_union_case_names(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_case_names, None,
            "an exception node must never seal a foreign union at its FQN"
        );
    }

    const MEASURE_TYPAR_FLAGS: i64 = 0b00000000100000000;

    fn make_typar(flags: i64) -> PickledTyparSpecData {
        PickledTyparSpecData {
            ident: PickledIdent {
                name: "t".to_string(),
                range: dummy_range(),
            },
            attribs: Vec::new(),
            flags,
            constraints: Vec::new(),
            xmldoc: PickledXmlDoc { lines: Vec::new() },
        }
    }

    #[test]
    fn is_measure_free_detects_measure_typars() {
        // FCS `TyparFlags.Kind`: a measure typar sets bit 8. Typar 0 is a type
        // parameter, typar 1 a measure.
        let mut ccu = make_ccu(Vec::new(), Vec::new(), 0);
        ccu.tables.typars = vec![make_typar(0), make_typar(MEASURE_TYPAR_FLAGS)];

        let non_generic = make_entity("A", PickledTyconRepr::NoRepr, empty_modul_typ());
        assert!(
            is_measure_free(&ccu, &non_generic),
            "no typars is measure-free"
        );

        let mut type_param = make_entity("B", PickledTyconRepr::NoRepr, empty_modul_typ());
        type_param.typars = vec![0];
        assert!(
            is_measure_free(&ccu, &type_param),
            "a type typar is measure-free"
        );

        let mut measure = make_entity("C", PickledTyconRepr::NoRepr, empty_modul_typ());
        measure.typars = vec![1];
        assert!(!is_measure_free(&ccu, &measure), "a measure typar is not");

        // An unknown typar index declines (the safe direction) rather than trust
        // a `typars.len()` we cannot validate.
        let mut dangling = make_entity("D", PickledTyconRepr::NoRepr, empty_modul_typ());
        dangling.typars = vec![99];
        assert!(
            !is_measure_free(&ccu, &dangling),
            "an unknown typar declines"
        );
    }

    #[test]
    fn a_measure_parameterised_hidden_union_is_declined() {
        // codex round 4: a `[<Measure>]` typar is erased from CLR metadata, so a
        // signature-hidden `type U<[<Measure>] 'u>` emits `U`1` with ZERO CLR
        // generic parameters — its `typars.len()` (1) matches no honest ECMA row
        // and, keyed against a same-name arity-1 overload or foreign union, would
        // seal the wrong entity. The seal declines a measure-parameterised
        // candidate (it keeps `None`), while a measure-free hidden union `V` at
        // the same namespace is still sealed.
        let mut u = make_entity("U", PickledTyconRepr::NoRepr, empty_modul_typ());
        u.typars = vec![0]; // one MEASURE typar
        let v = make_entity("V", PickledTyconRepr::NoRepr, empty_modul_typ()); // measure-free
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2, 3];
        let mut ns = make_entity("Ns", PickledTyconRepr::NoRepr, ns_modul);
        ns.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let mut root = make_entity("Ns", PickledTyconRepr::NoRepr, root_modul);
        root.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let mut pickled = make_ccu(vec![root, ns, u, v], Vec::new(), 0);
        pickled.tables.typars = vec![make_typar(MEASURE_TYPAR_FLAGS)];

        let mut entities = vec![
            make_ecma_entity(vec!["Ns"], "U", EntityKind::Union),
            make_ecma_entity(vec!["Ns"], "V", EntityKind::Union),
        ];
        apply_union_case_names(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_case_names, None,
            "a measure-parameterised hidden union is declined (kept unknowable)"
        );
        assert_eq!(
            entities[1].union_case_names.as_deref(),
            Some(&[][..]),
            "a measure-free hidden union is still sealed"
        );
    }

    #[test]
    fn a_projected_key_collision_declines_the_seal() {
        // codex round 6: borzoi's projected key `(strip_arity(name),
        // generic_parameters.len())` is not injective. A legal `type U` (opaque
        // sealed class) beside `[<CompiledName("U`0")>] type Other = A | B` yields
        // distinct metadata rows `U` and `U`0` that BOTH project to
        // `(name = "U", arity = 0)` — as do a measure-erased `U`1` beside a
        // non-generic `U`, and a `--staticlink` foreign union. Rather than
        // enumerate every such source (each review round found a new one), the
        // seal commits only when the key names EXACTLY ONE ECMA row; two rows
        // decline, leaving the real union's cases intact (`None` here — the row a
        // blind seal would have overwritten).
        let opaque = make_entity("U", PickledTyconRepr::NoRepr, empty_modul_typ());
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2];
        let mut ns = make_entity("Ns", PickledTyconRepr::NoRepr, ns_modul);
        ns.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let mut root = make_entity("Ns", PickledTyconRepr::NoRepr, root_modul);
        root.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
        let pickled = make_ccu(vec![root, ns, opaque], Vec::new(), 0);
        // Two ECMA rows collapse onto (Ns, "U", 0): the opaque class and a union.
        let mut entities = vec![
            make_ecma_entity(vec!["Ns"], "U", EntityKind::Class),
            make_ecma_entity(vec!["Ns"], "U", EntityKind::Union),
        ];
        apply_union_case_names(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[1].union_case_names, None,
            "a projected-key collision must decline, leaving the union's cases intact"
        );
    }

    // The following imports keep the symbol mentioned in module-level
    // docs alive even when no test directly constructs them.
    #[allow(dead_code)]
    fn _unused_keep_alive(
        _: FSharpTyparConstraint,
        _: Measure,
        _: PickledCPath,
        _: PickledTcRef,
        _: TupleKind,
        _: PickledILScopeRef,
        _: PickledXmlDoc,
    ) {
    }

    // -----------------------------------------------------------------
    // Entity `definition_range` overlay: stamping + arity-decline
    // -----------------------------------------------------------------
    //
    // These pin the range half of `apply_entity_overlay` on *synthetic* pickle
    // + ECMA trees, where each direction of the ambiguity guard can be isolated
    // (the F# compiler cannot express "one signature target, two metadata rows"
    // in source). The EntityRanges fixture pins the end-to-end shapes.

    /// A CCU whose header carries `strings` (so an entity range's file index
    /// resolves), rooted at stamp 0.
    fn ccu_with_strings(tycons: Vec<PickledEntity>, strings: Vec<String>) -> PickledCcu {
        let mut ccu = make_ccu(tycons, Vec::new(), 0);
        ccu.header.strings = strings;
        ccu
    }

    /// A namespace-`N` container holding the given child stamps, wrapped in the
    /// synthetic root — the `root(0) → N(1) → children` shape the overlay walks.
    fn root_and_namespace(children: Vec<u32>) -> [PickledEntity; 2] {
        let mut ns_modul = empty_modul_typ(); // IsType::Namespace
        ns_modul.entities = children;
        let ns = make_entity("N", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Asm", PickledTyconRepr::NoRepr, root_modul);
        [root, ns]
    }

    /// A top-level `type`-shaped pickle entity (`IsType::ModuleOrType`, object-
    /// model repr, `TyparKind::Type`) with `logical`/`compiled` names and an
    /// `entity_range` at `(file, line, col)` spanning one column.
    fn typed_entity_with_range(
        logical: &str,
        compiled: Option<&str>,
        file: u32,
        line: u32,
        col: u32,
    ) -> PickledEntity {
        let mut e = make_entity(
            logical,
            measure_object_model_repr(),
            module_modul_typ(vec![]),
        );
        e.compiled_name = compiled.map(str::to_string);
        e.range = PickledRange {
            file,
            start: PickledPos { line, column: col },
            end: PickledPos {
                line,
                column: col + 1,
            },
        };
        e
    }

    #[test]
    fn range_stamps_an_unambiguous_entity() {
        // stamps: root(0), N(1), A(2)
        let a = typed_entity_with_range("A", None, 0, 7, 4);
        let [root, ns] = root_and_namespace(vec![2]);
        let ccu = ccu_with_strings(vec![root, ns, a], vec!["Lib.fs".to_string()]);
        let mut owned = vec![make_ecma_entity(vec!["N"], "A", EntityKind::Class)];
        apply_entity_overlay(&mut owned, &ccu).unwrap();
        let range = owned[0]
            .definition_range
            .as_ref()
            .expect("unambiguous entity is stamped");
        assert_eq!(range.file, "Lib.fs");
        assert_eq!((range.start_line, range.start_column), (7, 4));
        assert_eq!((range.end_line, range.end_column), (7, 5));
    }

    #[test]
    fn range_declines_when_two_targets_collapse_onto_one_fqn() {
        // Collected-target-side guard, isolated: two pickle entities whose CLR
        // names both strip to `A` (`A`, and `[<CompiledName("A`1")>] B`), but a
        // *single* ECMA row `A` (so `find_entity_unique_mut` would happily hit
        // it). The stamp must still decline — two source types cannot both own
        // the one row. stamps: root(0), N(1), A(2), B(3).
        let a = typed_entity_with_range("A", None, 0, 1, 0);
        let b = typed_entity_with_range("B", Some("A`1"), 0, 2, 0);
        let [root, ns] = root_and_namespace(vec![2, 3]);
        let ccu = ccu_with_strings(vec![root, ns, a, b], vec!["Lib.fs".to_string()]);
        let mut owned = vec![make_ecma_entity(vec!["N"], "A", EntityKind::Class)];
        apply_entity_overlay(&mut owned, &ccu).unwrap();
        assert_eq!(
            owned[0].definition_range, None,
            "two collected targets on one FQN must decline"
        );
    }

    #[test]
    fn range_declines_when_two_ecma_rows_share_the_name() {
        // ECMA-side guard, isolated: a *single* pickle target `A` (collected
        // count 1), but two arity-stripped ECMA rows both named `A` — the shape
        // an `.fsi`-exported `type A` alongside a private `A<'T>` produces. The
        // name-only walk would hit whichever comes first; the unique-match guard
        // declines both. stamps: root(0), N(1), A(2).
        let a = typed_entity_with_range("A", None, 0, 1, 0);
        let [root, ns] = root_and_namespace(vec![2]);
        let ccu = ccu_with_strings(vec![root, ns, a], vec!["Lib.fs".to_string()]);
        let mut owned = vec![
            make_ecma_entity(vec!["N"], "A", EntityKind::Class),
            make_ecma_entity(vec!["N"], "A", EntityKind::Record),
        ];
        apply_entity_overlay(&mut owned, &ccu).unwrap();
        assert_eq!(owned[0].definition_range, None, "ambiguous ECMA row 0");
        assert_eq!(owned[1].definition_range, None, "ambiguous ECMA row 1");
    }

    #[test]
    fn range_declines_on_the_degenerate_unknown_file() {
        // A range whose file resolves to the synthetic `"unknown"` is declined
        // (probe finding 3 / D5), even though the FQN is unambiguous.
        let a = typed_entity_with_range("A", None, 0, 1, 0);
        let [root, ns] = root_and_namespace(vec![2]);
        let ccu = ccu_with_strings(vec![root, ns, a], vec!["unknown".to_string()]);
        let mut owned = vec![make_ecma_entity(vec!["N"], "A", EntityKind::Class)];
        apply_entity_overlay(&mut owned, &ccu).unwrap();
        assert_eq!(
            owned[0].definition_range, None,
            "unknown-file range declines"
        );
    }

    #[test]
    fn range_declines_on_a_bad_file_index() {
        // A dangling string-table index is a silent decline, not a panic.
        let a = typed_entity_with_range("A", None, 9, 1, 0);
        let [root, ns] = root_and_namespace(vec![2]);
        let ccu = ccu_with_strings(vec![root, ns, a], vec!["Lib.fs".to_string()]);
        let mut owned = vec![make_ecma_entity(vec!["N"], "A", EntityKind::Class)];
        apply_entity_overlay(&mut owned, &ccu).unwrap();
        assert_eq!(owned[0].definition_range, None, "bad file index declines");
    }
}
