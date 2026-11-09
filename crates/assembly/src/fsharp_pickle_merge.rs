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
    FSharpTyparConstraint, IsType, PickledAttribExpr, PickledAttribute, PickledCcu, PickledConst,
    PickledEntity, PickledExnRepr, PickledExpr, PickledMemberKind, PickledTcRef, PickledTyconRepr,
    PickledType, PickledUnionCase, PickledVal, TupleKind, TyparKind,
};
use crate::model::{
    AbbreviationTarget, Access, ArgGroup, ArgGroups, AssemblyIdentity, Augmentation, CaseFields,
    CompilerMessage, CurriedArg, CurriedArgs, Entity, EntityKind, EqualityComparison,
    FSharpConstraints, FsharpSourceRange, Member, MemberMeasures, MethodLike, NamedTypeHead,
    Obsolete, ReadValFacts, SkippedMember, TypeParameter, TypeRef, UnionCase, UnionCases,
    ValMeasures,
};
use crate::pickle_join::{
    Container, ContainerRows, ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE, JoinVerdict, PickleJoin,
    RowPath, visit_rows_mut,
};
use std::collections::HashMap;

/// The `ValFlags.IsExtensionMember` bit. FCS tests it as
/// `(flags &&& 0b00000000000100000000L) <> 0L` (`TypedTree.fs:192`); the
/// `val_flags` int64 is pickled un-masked (`PickledBits` keeps this bit),
/// so it survives verbatim into [`crate::fsharp_pickle::model::PickledVal::flags`].
const VAL_FLAGS_IS_EXTENSION_MEMBER: i64 = 0b1_0000_0000;

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
/// `OsgnTable::link` accepts as a no-op, so the
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

/// One entity the entity overlay stamps, plus the two host-pickle facts it
/// carries onto its row: its F# source name and its declaration range. Module
/// *member* source names are not this overlay's business: the pickle-driven
/// member list ([`apply_module_member_projection`]) sets them per claimed
/// member.
///
/// The two facts have **different participation**. Source-name stamping records
/// only module/type entities and always assigns (clearing a stale name is
/// correct). Range stamping additionally records measure leaves — a standalone
/// `[<Measure>] type m` pickles with an `IsType::Namespace` body, so it is a
/// type-chain *leaf* the module/type predicate misses — and a measure leaf's
/// `entity_source_name` is `None`, so letting it flow through source-name
/// stamping would clear a name it has no business clearing.
/// [`Self::is_module_or_type`] gates the source-name half accordingly.
struct EntityOverlayTarget {
    /// The entity's stamp, by which [`PickleJoin`] finds its row.
    stamp: u32,
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
/// module, a measure, an enum, an interface). Measure leaves are collected for
/// the range only: a standalone `[<Measure>] type m` pickles with an
/// `IsType::Namespace` body — a type-chain leaf the module/type predicate
/// misses — so the range collector also records the measure-leaf predicate
/// from [`merge_measure_entities`] (`typar_kind == Measure` with a backing
/// `FSharpObjectModel` repr), and [`EntityOverlayTarget::is_module_or_type`]
/// keeps it out of source-name stamping.
///
/// Both facts land only on the row [`PickleJoin`] certifies as the entity's
/// own, so twins whose CLR names strip alike (FSharp.Core's `FSharpExpr` and
/// ``FSharpExpr`1``) each get their own, and a row the join cannot attribute
/// keeps its sequence-point navigation and IL name.
///
/// Same single-CCU restriction as [`apply_module_member_projection`] — the
/// caller gates on the host pickle describing the whole image (see
/// `enumerate_type_defs`).
pub(crate) fn apply_entity_overlay(
    entities: &mut [Entity],
    pickled: &PickledCcu,
    join: &PickleJoin,
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
        &mut |stamp, entity, is_root, _namespace, _type_chain| {
            if is_root {
                return Ok(());
            }
            // A module/type participates in both facts. A measure leaf
            // (`typar_kind == Measure` + a backing TypeDef) is *not* a
            // module/type — it pickles an `IsType::Namespace` body — but has a
            // row of its own, so it contributes a range-only target. The two are
            // disjoint (a measure never has a module/type body), so an entity is
            // at most one kind.
            let is_module_or_type = matches!(
                entity.module_type.is_type,
                IsType::ModuleOrType | IsType::FSharpModuleWithSuffix
            );
            let is_measure_leaf = entity.typar_kind == TyparKind::Measure
                && matches!(entity.repr, PickledTyconRepr::FSharpObjectModel(_));
            if is_module_or_type || is_measure_leaf {
                targets.push(EntityOverlayTarget {
                    stamp,
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
    let rows = join.rows(entities);
    for target in targets {
        let Some(ecma) = rows.entity_mut(entities, target.stamp) else {
            continue;
        };
        if target.is_module_or_type {
            ecma.source_name = target.entity_source_name;
        }
        if let Some(range) = target.definition_range {
            ecma.definition_range = Some(range);
        }
    }
    Ok(())
}

/// One entity's F#-constraint readings, and how its row is found.
struct TyparConstraintTarget {
    /// The entity's stamp: [`PickleJoin`] finds a real type's row by it, and
    /// the container an abbreviation's marker hangs in.
    stamp: u32,
    /// For a type **abbreviation**, the name its marker is keyed by: the F#
    /// *source* name, which is how [`apply_abbreviation_markers`] keys the very
    /// rows an abbreviation target can match. Two aliases may legally share one
    /// `[<CompiledName>]` and still be distinct source types, so keying an alias
    /// by its CLR name would collapse a pair the marker pass deliberately kept
    /// apart. `None` for a real type.
    abbreviation_name: Option<String>,
    /// One reading per typar, in declaration order.
    readings: Vec<FSharpConstraints>,
}

/// Attach each F# entity's pickled typar constraints to its ECMA row
/// ([`FSharpConstraints`]).
///
/// Every typar of an F#-authored assembly was blanked to
/// [`FSharpConstraints::Unknowable`] before this runs, so this pass only ever
/// *licenses* a conclusion: a row it does not reach keeps the reading that
/// concludes nothing. That is what makes a marker-only assembly, a decode
/// failure and a foreign CCU all decline without any of them being
/// special-cased here.
///
/// A real type's row is the one [`PickleJoin`] certifies as its own; a
/// misattached `comparison` would silently license an application F# rejects.
///
/// An **abbreviation** is erased from IL, so it has no row of its own and the
/// only row it may claim is the name-only marker [`apply_abbreviation_markers`]
/// synthesised for it, found among the rows of the abbreviation's own container
/// by the marker's key — kind, source name and arity. A row that already
/// existed at that key is somebody *else's* (in a `--standalone` image, a
/// copied dependency's TypeDef), which is why the kind is part of the key: the
/// marker pass, which suppresses a marker whose name is taken, leaves exactly
/// that shape behind, and an unconstrained alias would otherwise report the
/// foreign type's constrained parameters as `Free`. Ambiguity is counted on
/// both sides: two aliases at one key in one container, or two markers there,
/// and neither is stamped.
pub(crate) fn apply_typar_constraints(
    entities: &mut Vec<Entity>,
    pickled: &PickledCcu,
    join: &PickleJoin,
) -> Result<(), ImportError> {
    let mut targets: Vec<TyparConstraintTarget> = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |stamp, entity, is_root, _namespace, _type_chain| {
            if is_root
                || !matches!(
                    entity.module_type.is_type,
                    IsType::ModuleOrType | IsType::FSharpModuleWithSuffix
                )
            {
                return Ok(());
            }
            if let Some(readings) = pickled_typar_constraints(pickled, entity) {
                targets.push(TyparConstraintTarget {
                    stamp,
                    abbreviation_name: entity
                        .type_abbrev
                        .is_some()
                        .then(|| abbreviation_lookup_name(entity)),
                    readings,
                });
            }
            Ok(())
        },
    )?;
    /// An abbreviation's marker key: its container, source name and arity.
    fn abbreviation_key<'t>(
        join: &PickleJoin,
        t: &'t TyparConstraintTarget,
    ) -> Option<(Option<Container>, &'t str, usize)> {
        t.abbreviation_name
            .as_deref()
            .map(|name| (join.container(t.stamp).cloned(), name, t.readings.len()))
    }
    let mut abbreviation_counts: HashMap<_, usize> = HashMap::new();
    for target in &targets {
        if let Some(key) = abbreviation_key(join, target) {
            *abbreviation_counts.entry(key).or_default() += 1;
        }
    }
    let rows = join.rows(entities);
    for target in &targets {
        let row = match &target.abbreviation_name {
            None => rows
                .entity_mut(entities, target.stamp)
                .filter(|e| e.generic_parameters.len() == target.readings.len()),
            Some(name) => {
                if abbreviation_key(join, target).is_none_or(|key| abbreviation_counts[&key] != 1) {
                    continue; // Two aliases at this key in this container.
                }
                let Some(ContainerRows { namespace, rows }) =
                    rows.container_rows_mut(entities, target.stamp)
                else {
                    continue;
                };
                let is_marker = |e: &Entity| {
                    namespace.as_ref().is_none_or(|ns| e.namespace == *ns)
                        && e.kind == EntityKind::Abbreviation
                        && e.source_name.as_deref().unwrap_or(&e.name) == name
                        && e.generic_parameters.len() == target.readings.len()
                };
                if rows.iter().filter(|e| is_marker(e)).count() != 1 {
                    continue; // Two rows at this key, or none.
                }
                rows.iter_mut().find(|e| is_marker(e))
            }
        };
        if let Some(ecma) = row {
            for (typar, reading) in ecma.generic_parameters.iter_mut().zip(&target.readings) {
                typar.fsharp_constraints = *reading;
            }
        }
    }
    Ok(())
}

/// The name an abbreviation's synthesised marker is keyed by: its F# *source*
/// name, which is the logical name whenever a `[<CompiledName>]` renamed it.
///
/// Derived exactly as [`apply_abbreviation_markers`] derives the `source_name`
/// it stamps on the marker, because the two must agree: this is the name that
/// pass uses to decide whether a marker is needed at all, so it is the name the
/// resulting row carries.
fn abbreviation_lookup_name(entity: &PickledEntity) -> String {
    match &entity.compiled_name {
        Some(compiled) if *compiled != entity.logical_name => {
            strip_arity(&entity.logical_name).to_string()
        }
        _ => clr_name(entity),
    }
}

/// What one typar's pickled constraint list says ([`FSharpConstraints`]): free
/// when it is empty; which of `equality` and `comparison` it holds when it holds
/// nothing else; and bare presence otherwise.
///
/// `DefaultsTo` restricts nothing — it only picks a default for an unresolved
/// typar — but it reads as `Present` all the same, and so declines where it
/// need not: a coverage loss inside a sound direction.
fn constraint_reading(constraints: &[FSharpTyparConstraint]) -> FSharpConstraints {
    if constraints.is_empty() {
        return FSharpConstraints::Free;
    }
    let mut supports = EqualityComparison {
        equality: false,
        comparison: false,
    };
    for c in constraints {
        match c {
            FSharpTyparConstraint::SupportsEquality => supports.equality = true,
            FSharpTyparConstraint::SupportsComparison => supports.comparison = true,
            _ => return FSharpConstraints::Present,
        }
    }
    FSharpConstraints::Supports(supports)
}

/// One [`FSharpConstraints`] per pickled typar of `entity`, in declaration
/// order, or `None` when the pickled positions cannot be matched to the ECMA
/// row's generic parameters.
///
/// Each typar's list is read by [`constraint_reading`].
///
/// `None` (hence `Unknowable`) for a measure-bearing entity: an erased
/// `[<Measure>]` typar has no CLR generic parameter, so the pickled list is
/// longer than the ECMA one and position `i` names different typars on the two
/// sides. [`is_measure_free`] also treats an out-of-range typar index as a
/// measure, so a corrupt index declines here rather than reading a neighbour's
/// constraints.
fn pickled_typar_constraints(
    pickled: &PickledCcu,
    entity: &PickledEntity,
) -> Option<Vec<FSharpConstraints>> {
    if !is_measure_free(pickled, entity) {
        return None;
    }
    entity
        .typars
        .iter()
        .map(|&ti| {
            pickled
                .tables
                .typars
                .get(ti as usize)
                .map(|t| constraint_reading(&t.constraints))
                // Unreachable while `is_measure_free` treats an unknown index as
                // a measure, but the `Option` is what makes that a decline
                // rather than a panic if that ever changes.
                .ok_or(())
        })
        .collect::<Result<Vec<_>, ()>>()
        .ok()
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
/// [`MethodLike::augmentation`](crate::MethodLike::augmentation)
/// fact, which is exact per projected member rather than keyed by name (a module
/// may hold both a `let M` and an augmentation `M`; only the latter is hidden).
///
/// Scoped to the declaring module exactly as the other overlays are: each
/// module's row is the one [`PickleJoin`] certifies as its own. A module whose
/// row the join cannot reach (filtered out, dropped, or not attributable) is
/// skipped; like the extension flag, this list is an annotation, not a
/// structural claim, so a missing row under-populates rather than hard-errors.
/// The list's *completeness across the assembly* is bounded by
/// [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`](crate::AssemblyProjectionSkips::fsharp_abbreviations_unknowable)
/// — a foreign-CCU or undecodable image may declare extension members in modules
/// the host pickle never describes, which the consumer treats as unknowable.
pub(crate) fn apply_extension_member_index(
    entities: &mut [Entity],
    pickled: &PickledCcu,
    join: &PickleJoin,
) -> Result<(), ImportError> {
    // Each module's stamp, with its instance and static extension names (EX-0).
    let mut targets: Vec<(u32, Vec<String>, Vec<String>)> = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |stamp, entity, is_root, _namespace, _type_chain| {
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
                    targets.push((stamp, instance, statics));
                }
            }
            Ok(())
        },
    )?;
    let rows = join.rows(entities);
    for (stamp, instance, statics) in targets {
        if let Some(ecma) = rows.entity_mut(entities, stamp) {
            ecma.extension_member_names = instance;
            ecma.static_extension_member_names = statics;
        }
    }
    Ok(())
}

/// Keep exactly those of a union's candidate properties the host pickle
/// **publishes**, dropping the rest.
///
/// [`Ecma335Assembly::project_fsharp_members`](crate::Ecma335Assembly) keeps a
/// union's property rows wholesale because the metadata cannot answer which of
/// them a consumer may name. fsc emits an `Is<Case>` property per case, plus the
/// `Tag` discriminant, all carrying the same attributes
/// (`[CompilerGenerated]`, `[DebuggerNonUserCode]`, `[DebuggerBrowsable]`) — and
/// it emits the `Is<Case>` rows *whether or not the language version publishes
/// them*. Under `<LangVersion>8.0</LangVersion>` the rows are byte-identical to
/// a current one's, yet `x.IsA` is `FS0039`. So no property of the row
/// distinguishes a member from a hidden helper, and neither does the case list:
/// the cases exist in both.
///
/// `published` is the entity's pickled `tcaug.adhoc` names, which is what FCS
/// itself reads (`MembersOfFSharpTyconSorted`) — the one place the answer is
/// recorded. Members are pickled under an accessor's own name, so a property `P`
/// is vouched for by `get_P` or `set_P`.
///
/// Settle the candidate properties `project_fsharp_members` keeps on every F#
/// record and exception: a user-defined member property (`member p.Sum`) is a
/// member FCS surfaces, while the rows fsc synthesises beside it are not, and
/// metadata alone cannot tell them apart. The host pickle's published member
/// list (`tcaug.adhoc`, as [`retain_published_union_properties`] reads it for a
/// union) decides: each record or exception row the pickle describes keeps the
/// properties it vouches for. Every other record or exception row — with no
/// pickle (`pickled` is `None`, or not authoritative), one [`PickleJoin`] cannot
/// attribute, or a measure-parameterised type — keeps none: a decline, exactly
/// what the projection surfaced before these were candidates. A field-backed
/// property is a [`Member::Field`] and is not touched.
pub(crate) fn settle_record_properties(
    entities: &mut [Entity],
    pickled: Option<(&PickledCcu, &PickleJoin)>,
) -> Result<(), ImportError> {
    // The published names of each record or exception row the pickle vouches
    // for, by the row's identity.
    let mut published: HashMap<RowPath, Vec<String>> = HashMap::new();
    if let Some((pickled, join)) = pickled {
        let mut path = Vec::new();
        walk_entity_tree(
            pickled,
            pickled.root_entity,
            true,
            &[],
            &[],
            &mut path,
            &mut |stamp, entity, is_root, _namespace, _type_chain| {
                let is_record_or_exception = matches!(entity.repr, PickledTyconRepr::Record(_))
                    || matches!(entity.exn_repr, PickledExnRepr::Fresh(_))
                    || matches!(
                        &entity.repr,
                        PickledTyconRepr::FSharpObjectModel(m)
                            if matches!(m.kind, crate::fsharp_pickle::model::PickledTyconObjModelKind::Record)
                    );
                if is_root || !is_record_or_exception || !is_measure_free(pickled, entity) {
                    return Ok(());
                }
                if let Some(row) = join.joined_path(stamp) {
                    let names = entity
                        .tcaug
                        .adhoc
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect();
                    published.insert(row.clone(), names);
                }
                Ok(())
            },
        )?;
    }
    visit_rows_mut(entities, &mut |row, e| {
        if matches!(e.kind, EntityKind::Record | EntityKind::Exception) {
            let vouched = row.and_then(|row| published.get(row));
            retain_published_union_properties(e, vouched.map_or(&[], Vec::as_slice));
        }
    });
    Ok(())
}

/// Only properties are filtered; a union's methods and fields are settled
/// elsewhere. Passing an empty `published` drops every property, which is the
/// honest reading whenever the pickle vouches for nothing.
fn retain_published_union_properties(entity: &mut Entity, published: &[String]) {
    entity.members.retain(|m| match m {
        Member::Property(p) => {
            let getter = format!("get_{}", p.name);
            let setter = format!("set_{}", p.name);
            published.iter().any(|n| *n == getter || *n == setter)
        }
        _ => true,
    });
}

/// Populate each F# union entity's
/// [`union_cases`](crate::Entity::union_cases) — the case names in
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
/// and the resulting `UnionCases::Known(vec![])` is a real
/// observation, distinct from `UnionCases::Unknowable` (codex round 21).
///
/// A union whose representation is **hidden by a signature** (`type Teq<'a,'b>`
/// exposed opaquely in a `.fsi` while the `.fs` defines the union, or an inline
/// `[<Sealed>]` signature) pickles its SIGNATURE with `NoRepr` — no union repr
/// at all — yet the compiled class keeps `CompilationMapping(SumType)`, so the
/// ECMA projector still classifies it `EntityKind::Union`. Such an entity is not
/// reached by the repr walk above; the second loop seals it to the same
/// knowably-empty `UnionCases::Known(vec![])` (a signature-hidden representation exposes zero
/// accessible cases), keyed on the ECMA `Union` kind and restricted to opaque,
/// measure-free *type* nodes (never modules / namespaces / abbreviations /
/// exceptions / measure-parameterised types).
///
/// Both passes reach a union's row through [`PickleJoin`], so the cases and
/// members land only on the row the union compiled to — never on a sibling
/// whose CLR name strips alike (``[<CompiledName("U`0")>]`` beside `U`), nor on
/// a surviving squatter when the union's own row was dropped. The real-case
/// pass (first loop) additionally requires the row to be an
/// `EntityKind::Union`, since the pickle says it is one and a row that
/// disagrees is not one this overlay can describe; that matters because the
/// property retention is destructive.
///
/// A measure-parameterised union is left alone by both passes: its case fields
/// may mention an erased `[<Measure>]` typar, which has no CLR generic
/// parameter to line up with.
///
/// The real-case pass (the first loop) is a host-CCU fact, so it runs even when
/// the source-name overlay is non-authoritative (foreign pickles present) — like
/// the abbreviation markers and the extension-member index. Cross-assembly
/// completeness is bounded by
/// [`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`](crate::AssemblyProjectionSkips::fsharp_abbreviations_unknowable):
/// a foreign-CCU union keeps an empty list, which consumers must read as
/// *unknowable* (a union has at least one case by construction). A union
/// whose ECMA row the join cannot reach is skipped — an annotation, not a
/// structural claim, exactly as the extension index treats a missing row.
pub(crate) fn apply_union_cases(
    entities: &mut [Entity],
    pickled: &PickledCcu,
    join: &PickleJoin,
) -> Result<(), ImportError> {
    // (stamp, cases, published members) per union-repr entity.
    let mut targets: Vec<(u32, Vec<UnionCase>, Vec<String>)> = Vec::new();
    // A union whose representation is HIDDEN BY A SIGNATURE pickles as `NoRepr`
    // (see the second loop below) — the union-repr match above never reaches it.
    // Collect every opaque type's stamp so a still-unknowable ECMA `Union` row
    // of its own can be sealed to a knowably-empty case list.
    let mut hidden_repr: Vec<(u32, Vec<String>)> = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |stamp, entity, is_root, _namespace, _type_chain| {
            let cases = match &entity.repr {
                PickledTyconRepr::Union(cases) => Some(cases),
                PickledTyconRepr::UnionWithStaticFields { cases, .. } => Some(cases),
                _ => None,
            };
            // The CCU wrapper (`is_root`) is never a target on either side.
            if is_root || !is_measure_free(pickled, entity) {
                return Ok(());
            }
            // The entity's PUBLISHED members, by the name each is pickled under
            // — an accessor's own (`get_IsOne`), not its property's. This is
            // what FCS reads to answer `x.IsOne` (`MembersOfFSharpTyconSorted`),
            // and it is the only place the answer exists: fsc emits the
            // `Is<Case>` metadata properties whether or not the language version
            // publishes them, identically attributed either way. Hiding the
            // REPRESENTATION does not hide the members: a `.fsi` that exposes
            // `type U` opaquely may still publish `member IsFoo: bool`, and FCS
            // binds `x.IsFoo` against it — so the opaque path reads it too.
            let published = || {
                entity
                    .tcaug
                    .adhoc
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect()
            };
            if let Some(cases) = cases {
                targets.push((
                    stamp,
                    cases
                        .iter()
                        // Only `TAccess []` (public) cases: a private
                        // representation (`type U = private | Hidden`) pickles
                        // each case with a restricted access path, and a
                        // cross-assembly consumer can never name it — listing it
                        // would let a hidden case wrongly shadow a same-named
                        // earlier binding FCS resolves (codex round 21).
                        // Filtering can leave the list EMPTY, which is a real
                        // observation ("knowably zero accessible cases"),
                        // distinct from the absent-pickle `None`.
                        .enumerate()
                        .filter(|(_, c)| c.access.is_empty())
                        .map(|(i, c)| UnionCase {
                            name: c.ident.name.clone(),
                            // The pickle lists every case, accessible or not, in
                            // declaration order: the position fsc writes on the
                            // case's maker.
                            declaration_index: u32::try_from(i)
                                .expect("a union has fewer than 2^32 cases"),
                            fields: case_fields(pickled, c),
                        })
                        .collect(),
                    published(),
                ));
            } else if entity.flags & ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE == 0
                && entity.type_abbrev.is_none()
                && matches!(entity.exn_repr, PickledExnRepr::None)
                && matches!(entity.repr, PickledTyconRepr::NoRepr)
            {
                // An OPAQUE TYPE: a *type* (the `IsModuleOrNamespace` flag bit is
                // clear — so not a module or namespace), not an abbreviation, not
                // an exception, whose representation is `NoRepr`. A
                // signature-hidden union is exactly this shape (a hidden class or
                // record lands here too, but its ECMA kind is not `Union`, so the
                // second loop's filter skips it).
                hidden_repr.push((stamp, published()));
            }
            Ok(())
        },
    )?;
    let rows = join.rows(entities);
    for (stamp, names, published) in targets {
        if let Some(ecma) = rows
            .entity_mut(entities, stamp)
            .filter(|e| e.kind == EntityKind::Union)
        {
            ecma.union_cases = UnionCases::Known(names);
            retain_published_union_properties(ecma, &published);
        }
    }
    // Signature-hidden unions. `type Teq<'a,'b>` exposed opaquely in a `.fsi`
    // while the `.fs` has `type Teq<'a,'b> = private Teq of …` (or an inline
    // `[<Sealed>]` signature) replaces the impl's union repr with `TNoRepr` in
    // the SIGNATURE data (FCS `SignatureConformance`: `TFSharpTyconRepr r,
    // TNoRepr`), so the walk above never reached it. But the compiled class
    // still carries `CompilationMapping(SumType)`, so the ECMA projector
    // classifies it `EntityKind::Union` — and it kept `UnionCases::Unknowable`,
    // which the module-open fold (`sema` `fold_tycon_tier`) reads as "unknowable
    // hidden cases" and defers every dotted head over (a bare `List.replicate`
    // where a file-local union case `List` forces the dotted-path branch). A
    // representation hidden by the signature exposes ZERO accessible cases, so
    // the honest answer is a knowably-empty `UnionCases::Known(vec![])`, exactly
    // like the private-case filter above.
    //
    // The row sealed is the one the join certifies as the opaque type's own.
    // That is what is load-bearing here: the seal concludes "no accessible
    // cases", and on any other row — a `--staticlink` foreign union at a name
    // that strips alike, or a squatter beside a dropped own row — that is a
    // wrong answer, not a decline. It is sound without trusting
    // `authoritative`, which a `--nointerfacedata` dependency defeats (its
    // copied TypeDefs bring no signature resource), because the join counts
    // every row of the image, foreign ones included.
    for (stamp, published) in hidden_repr {
        if let Some(ecma) = rows
            .entity_mut(entities, stamp)
            .filter(|e| e.kind == EntityKind::Union && e.union_cases == UnionCases::Unknowable)
        {
            ecma.union_cases = UnionCases::Known(Vec::new());
            retain_published_union_properties(ecma, &published);
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

/// Where a val's receiver sits in the IL method it compiles to, which decides
/// how its `ValReprInfo`'s first group is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiverAt {
    /// No receiver: a module `let`, a static member, a static augmentation, or
    /// a constructor.
    None,
    /// The IL `this` of an intrinsic instance member.
    This,
    /// The first IL parameter of an instance augmentation, which compiles to a
    /// static of its module.
    LeadingParam,
}

/// The val's argument groups as a caller writes them ([`CurriedArgs`]), with
/// each argument's pickled F# name: `None` when it carries no `ValReprInfo`, or
/// when an instance val's first group is not the single receiver the
/// definition must have put there.
fn curried_args(v: &PickledVal, receiver: ReceiverAt) -> Option<CurriedArgs> {
    let repr = v.repr_info.as_ref()?;
    let group = |args: &[crate::fsharp_pickle::model::PickledArgReprInfo]| ArgGroup {
        args: args
            .iter()
            .map(|a| CurriedArg {
                name: a.name.as_ref().map(|i| i.name.clone()),
            })
            .collect(),
    };
    let groups = match receiver {
        ReceiverAt::None => &repr.arg_repr[..],
        ReceiverAt::This | ReceiverAt::LeadingParam => match repr.arg_repr.split_first() {
            Some((this, rest)) if this.len() == 1 => rest,
            _ => return None,
        },
    };
    Some(CurriedArgs {
        leading_receiver: receiver == ReceiverAt::LeadingParam,
        groups: groups.iter().map(|g| group(g)).collect(),
    })
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
    /// The module's stamp — its index into the pickle's OSGN tycon table
    /// ([`PickledCcu`]`.tables.tycons`) — by which the projector finds the
    /// module's own row.
    pub stamp: u32,
    /// The entity's namespace prefix, from the pickle walk's namespace vs.
    /// type-chain split (see `walk_entity_tree`). Together with
    /// [`Self::type_chain`] a readable label, not an identity: CLR names that
    /// strip alike share it.
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
    /// The val's argument groups as a caller writes them, each argument with
    /// its F# name (`None` when it carries no `ValReprInfo`). An instance
    /// augmentation's receiver group is the
    /// [`leading_receiver`](CurriedArgs::leading_receiver) parameter rather
    /// than a group, so this has one group fewer than
    /// [`arg_group_count`](Self::arg_group_count) counts there.
    pub curried: Option<CurriedArgs>,
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
    /// For an augmentation, how many type parameters its target type has —
    /// see [`Augmentation::Certain`]. `None` for a plain module `let`.
    pub target_typars: Option<usize>,
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
    /// The documentation-comment ID fsc recorded for the val (`p_ValData`'s
    /// `XmlDocSig`) — the key it wrote into the doc XML. `None` for an
    /// undocumented val, or one compiled without a doc file, both of which
    /// pickle an empty string.
    pub xml_doc_sig: Option<String>,
    /// How many of the val's type parameters are units of measure: IL erases
    /// them, but the doc key's generic arity counts them.
    pub measure_typars: usize,
    /// One [`FSharpConstraints`] reading per type parameter of the val's F#
    /// type, in the order its quantifier binds them — the order fsc emits the
    /// IL method's generic parameters in. `None` when the positions cannot be
    /// trusted to correspond: a type parameter that is a unit of measure (IL
    /// erases it, so the pickled list is longer than the IL one), or one whose
    /// index does not resolve.
    pub typar_constraints: Option<Vec<FSharpConstraints>>,
    /// What the val says about a use of its method beyond the IL: whether its
    /// F# type mentions a unit of measure, and the attributes F# checks at a
    /// use. [`crate::ValFacts::Unknowable`] when a type parameter's index does
    /// not resolve or an attribute's class cannot be named.
    pub val_facts: crate::model::ValFacts,
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
        &mut |stamp, entity, is_root, namespace, type_chain| {
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
                    stamp,
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
        let (typar_constraints, measures) = val_type_facts(pickled, v)?;
        let val_facts = match (measures, val_use_attributes(pickled, v)) {
            (Some(measures), Some((compiler_message, requires_explicit_type_arguments))) => {
                crate::model::ValFacts::Read(Box::new(ReadValFacts {
                    measures,
                    compiler_message,
                    requires_explicit_type_arguments,
                }))
            }
            _ => crate::model::ValFacts::Unknowable,
        };
        out.push(ModuleMemberVal {
            val_index: vi,
            logical_name: v.logical_name.clone(),
            compiled_name: v.compiled_name.clone(),
            il_arity: val_il_arity(v),
            arg_group_count: v.repr_info.as_ref().map(|r| r.arg_repr.len()),
            curried: curried_args(
                v,
                if v.member_info
                    .as_ref()
                    .is_some_and(|mi| mi.flags.is_instance)
                {
                    ReceiverAt::LeadingParam
                } else {
                    ReceiverAt::None
                },
            ),
            is_member: v.member_info.is_some(),
            is_instance: v
                .member_info
                .as_ref()
                .is_some_and(|mi| mi.flags.is_instance),
            is_extension,
            target_typars: augmentation_target_typars(pickled, v),
            // Only a `Type`-kinded typar makes the IL method generic;
            // measure typars are erased from IL.
            is_generic: v
                .repr_info
                .as_ref()
                .is_some_and(|r| r.typar_repr.iter().any(|t| t.kind == TyparKind::Type)),
            is_literal: v.literal_value.is_some(),
            is_public: v.access.is_empty(),
            definition_range: resolve_definition_range(pickled, v),
            xml_doc_sig: pickled_xml_doc_sig(&v.xmldoc_sig),
            measure_typars: measure_typar_count(v),
            typar_constraints,
            val_facts,
        });
    }
    Ok(out)
}

/// How many type parameters a member val's apparent parent has: for an
/// augmentation, the leading generic parameters of the module method it
/// compiles to, which are its target type's ([`Augmentation::Certain`]).
///
/// A local parent's are its pickled typars. A non-local one is a path of
/// logical names, which carry a generic type's arity as a `` `N `` suffix
/// (`List`1`, and for a nested IL type each level its own:
/// `Dictionary`2` / `KeyCollection` is two). `None` for a val with no member
/// info, an index that does not resolve, or a val generic over a unit of
/// measure: IL erases the measure, so the pickled count is not the IL one.
fn augmentation_target_typars(pickled: &PickledCcu, v: &PickledVal) -> Option<usize> {
    let info = v.member_info.as_ref()?;
    if measure_typar_count(v) > 0 {
        return None;
    }
    match info.apparent_parent {
        PickledTcRef::Local(stamp) => pickled
            .tables
            .tycons
            .get(stamp as usize)
            .map(|parent| parent.typars.len()),
        PickledTcRef::NonLocal(index) => {
            let nle = pickled.header.nlerefs.get(index as usize)?;
            nle.path
                .iter()
                .map(|&i| logical_name_arity(pickled.header.strings.get(i as usize)?))
                .sum()
        }
    }
}

/// The generic arity a logical name's `` `N `` suffix records, 0 without one:
/// FCS's `DemangleGenericTypeName`, which reads the digits after the last
/// backtick. `None` for a count that does not fit.
fn logical_name_arity(name: &str) -> Option<usize> {
    match name.rsplit_once('`') {
        Some((_, digits)) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
            digits.parse().ok()
        }
        _ => Some(0),
    }
}

/// What a val's pickled F# type says that its IL signature cannot: each type
/// parameter's [`FSharpConstraints`], and what the type says about units of
/// measure ([`ValMeasures`]). The two are read together because a measure
/// decides both — a measure type parameter is erased from IL, so the
/// constraint readings no longer line up with the IL generic parameters, and
/// the IL signature is no longer the F# type.
///
/// A type parameter whose index does not resolve, or a type shape the scan
/// does not model, leaves both unknown: a measure could hide behind it. Each
/// constraint list is read by [`constraint_reading`], as an entity's is
/// ([`pickled_typar_constraints`]). A dangling table index elsewhere in the
/// type is a malformed pickle — a loud `Err`.
fn val_type_facts(pickled: &PickledCcu, v: &PickledVal) -> Result<ValTypeFacts, ImportError> {
    let (typars, body): (&[u32], &PickledType) = match &v.ty {
        PickledType::Forall { typars, body } => (typars, body),
        ty => (&[], ty),
    };
    let mut readings = Vec::with_capacity(typars.len());
    for &ti in typars {
        let Some(typar) = pickled.tables.typars.get(ti as usize) else {
            return Ok((None, None));
        };
        if is_measure_typar(typar.flags) {
            return Ok((None, Some(ValMeasures::Measured)));
        }
        readings.push(constraint_reading(&typar.constraints));
    }
    // Heads are collected whatever the val's accessibility: a non-public
    // function can mention a non-public type, whose head then resolves for no
    // consumer and declines — the cheap sound reading.
    let mut scan = ValMeasureScan::default();
    scan.scan(pickled, body)?;
    Ok(match scan.reading() {
        Some(measures) => (Some(readings), Some(measures)),
        None => (None, None),
    })
}

/// What the pickle says of one union case's fields ([`CaseFields`]): how
/// many, whether a unit of measure appears in their types (the scan a module
/// function's val gets, over the fields in order), and whether the case is
/// attributed. A type the scan cannot read leaves the fields unknowable.
fn case_fields(pickled: &PickledCcu, case: &PickledUnionCase) -> CaseFields {
    let mut scan = ValMeasureScan::default();
    for field in &case.fields {
        if scan.scan(pickled, &field.ty).is_err() {
            return CaseFields::Unknowable;
        }
    }
    match scan.reading() {
        Some(measures) => CaseFields::Read {
            count: case.fields.len(),
            measures,
            attributed: !case.attribs.is_empty(),
        },
        None => CaseFields::Unknowable,
    }
}

/// [`val_type_facts`]'s result: the constraint readings and the measure
/// reading, each `None` when unknown.
type ValTypeFacts = (Option<Vec<FSharpConstraints>>, Option<ValMeasures>);

/// What one scan over pickled types found. A measure, and an unreadable
/// shape, are each final; the heads are what the consumer must clear when
/// neither was found.
#[derive(Debug, Default)]
struct ValMeasureScan {
    measured: bool,
    unknowable: bool,
    heads: std::collections::BTreeSet<NamedTypeHead>,
}

impl ValMeasureScan {
    /// The reading: `None` when a shape was unreadable and no measure was
    /// found.
    fn reading(self) -> Option<ValMeasures> {
        if self.measured {
            Some(ValMeasures::Measured)
        } else if self.unknowable {
            None
        } else {
            Some(ValMeasures::NoDirectMeasure {
                heads: self.heads.into_iter().collect(),
            })
        }
    }

    /// Scan one pickled type.
    fn scan(&mut self, pickled: &PickledCcu, ty: &PickledType) -> Result<(), ImportError> {
        match ty {
            PickledType::Measure(_) => self.measured = true,
            PickledType::Var { typar_index, .. } => {
                self.typar(pickled, *typar_index);
            }
            PickledType::Forall { typars, body } => {
                for &ti in typars {
                    self.typar(pickled, ti);
                }
                self.scan(pickled, body)?;
            }
            PickledType::AppSimple {
                simpletyp_index, ..
            } => {
                let nle = *pickled
                    .header
                    .simpletys
                    .get(*simpletyp_index as usize)
                    .ok_or(ImportError::OsgnIndexOutOfRange {
                        kind: "simplety (measures)",
                        index: *simpletyp_index,
                        max: pickled.header.simpletys.len(),
                    })?;
                self.nonlocal_head(pickled, nle)?;
            }
            PickledType::App { tcref, args, .. } => {
                match tcref {
                    PickledTcRef::NonLocal(nle) => self.nonlocal_head(pickled, *nle)?,
                    // fsc names every type a signature mentions by a non-local
                    // reference, a same-assembly one included: the 1,481
                    // F#-authored assemblies of one NuGet cache carry no
                    // `Local` reference among the 2,092,632 heads of their
                    // module vals' types (nor among their member vals', record
                    // fields' or abbreviations'). One would come from a pickle
                    // this reader was not measured against.
                    PickledTcRef::Local(_) => self.unknowable = true,
                }
                for arg in args {
                    self.scan(pickled, arg)?;
                }
            }
            PickledType::Fun { domain, range, .. } => {
                self.scan(pickled, domain)?;
                self.scan(pickled, range)?;
            }
            PickledType::Tuple { elems, .. } => {
                for elem in elems {
                    self.scan(pickled, elem)?;
                }
            }
            // A union-case type occurs inside expressions, not in a declared
            // type.
            PickledType::UCase { .. } => self.unknowable = true,
        }
        Ok(())
    }

    /// A type parameter is a measure (or does not resolve, so might be one).
    fn typar(&mut self, pickled: &PickledCcu, index: u32) {
        match pickled.tables.typars.get(index as usize) {
            Some(t) if is_measure_typar(t.flags) => self.measured = true,
            Some(_) => {}
            None => self.unknowable = true,
        }
    }

    fn nonlocal_head(&mut self, pickled: &PickledCcu, nle: u32) -> Result<(), ImportError> {
        match decode_nonlocal_named(pickled, nle, Vec::new())? {
            AbbreviationTarget::Named { ccu, path, .. } => {
                self.heads.insert(NamedTypeHead { ccu, path });
            }
            _ => unreachable!("`decode_nonlocal_named` builds a `Named` target"),
        }
        Ok(())
    }
}

/// The attributes F# checks at a use of a val, read off its pickled
/// attribute list: its `[<CompilerMessage>]`, and whether it carries
/// `[<RequiresExplicitTypeArguments>]`. `None` when an attribute's class
/// cannot be named, since that attribute could be either.
///
/// fsc pickles every attribute class as a **non-local** reference naming its
/// CCU and path, even a class declared in the same assembly: FSharp.Core's own
/// `CompilerMessage` on `nameof` is a reference to `FSharp.Core`, and the 1,481
/// F#-authored assemblies of one NuGet cache carry no `Local` attribute
/// reference among the 753,007 on their vals. A `Local` one would come from a
/// pickle this reader was not measured against, so it leaves the facts unknown.
///
/// The class is matched by path alone, not by CCU. Both facts only ever make a
/// use decline, so an assembly declaring its own class at either path is read
/// more cautiously than FCS reads it, never less.
fn val_use_attributes(
    pickled: &PickledCcu,
    v: &PickledVal,
) -> Option<(Option<CompilerMessage>, bool)> {
    const COMPILER_MESSAGE: [&str; 4] = ["Microsoft", "FSharp", "Core", "CompilerMessageAttribute"];
    const REQUIRES_EXPLICIT_TYPE_ARGUMENTS: [&str; 4] = [
        "Microsoft",
        "FSharp",
        "Core",
        "RequiresExplicitTypeArgumentsAttribute",
    ];
    let mut compiler_message = None;
    let mut requires_explicit_type_arguments = false;
    for attr in &v.attribs {
        let PickledTcRef::NonLocal(idx) = attr.tcref else {
            return None;
        };
        let nle = pickled.header.nlerefs.get(idx as usize)?;
        let class = nle
            .path
            .iter()
            .map(|&i| pickled.header.strings.get(i as usize).map(String::as_str))
            .collect::<Option<Vec<&str>>>()?;
        if class == COMPILER_MESSAGE {
            compiler_message = Some(decode_compiler_message(attr));
        } else if class == REQUIRES_EXPLICIT_TYPE_ARGUMENTS {
            requires_explicit_type_arguments = true;
        }
    }
    Some((compiler_message, requires_explicit_type_arguments))
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

/// Whether a physical method implements the claim slot `(name, shape)`: a
/// value binding by its static getter, a function by name and arity, a val
/// without `ValReprInfo` by name.
fn physical_claim_slot_matches(m: &PhysicalMethod, name: &str, shape: ClaimShape) -> bool {
    match shape {
        ClaimShape::Value => {
            m.name.strip_prefix("get_") == Some(name) && m.params.is_none_or(|n| n == 0)
        }
        ClaimShape::Function(n) => m.name == name && m.params.is_none_or(|p| p == n),
        ClaimShape::Unknown => m.name == name,
    }
}

/// Whether a projected member occupies the claim slot `(name, shape)`.
fn claim_slot_matches(slot: &Option<Member>, name: &str, shape: ClaimShape) -> bool {
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
        target_typars: v.target_typars,
        definition_range: v.definition_range.clone(),
        arg_group_count: callable_arg_group_count(v),
        curried: v.curried.clone(),
        xml_doc_sig: v.xml_doc_sig.clone(),
        // Read for a plain module function only. An augmentation compiles to
        // a module static whose generic parameters also carry its target
        // type's, an order this does not establish, so its readings stay
        // unknown.
        typar_constraints: if v.is_member {
            None
        } else {
            v.typar_constraints.clone()
        },
        use_facts: if v.is_member {
            crate::model::ValFacts::Unknowable
        } else {
            v.val_facts.clone()
        },
    }
}

/// How many of a val's type parameters are units of measure — erased from IL,
/// but counted in its doc key's generic arity.
fn measure_typar_count(v: &PickledVal) -> usize {
    v.repr_info.as_ref().map_or(0, |r| {
        r.typar_repr
            .iter()
            .filter(|t| t.kind == TyparKind::Measure)
            .count()
    })
}

/// A pickled `XmlDocSig` as the model carries it: `None` for the empty string
/// fsc pickles for an undocumented item (or for every item, when it wrote no
/// doc file).
pub(crate) fn pickled_xml_doc_sig(sig: &str) -> Option<String> {
    (!sig.is_empty()).then(|| sig.to_string())
}

/// The val's argument groups as a *caller* counts them.
///
/// `ValReprInfo.arg_repr` counts the groups the **definition** has, which for an
/// instance augmentation includes the receiver — `type String with member
/// x.GenericExt y` is two groups there and one at the call site. `arg_group_count`
/// is consumed as the callable count (the overload engine asks "could this be
/// curried?" of the argument list a call writes), so drop the receiver group.
fn callable_arg_group_count(v: &ModuleMemberVal) -> Option<usize> {
    match (v.arg_group_count, v.is_instance) {
        (Some(groups), true) => groups.checked_sub(1),
        (count, false) => count,
        (None, true) => None,
    }
}

/// The facts [`val_facts`] reads off one val.
struct ValFacts {
    source_name: Option<String>,
    instance_extension: bool,
    fsharp_extension: bool,
    /// See [`ModuleMemberVal::target_typars`].
    target_typars: Option<usize>,
    definition_range: Option<FsharpSourceRange>,
    /// See [`callable_arg_group_count`]. A *group* fact like the rest: which val
    /// claimed which MethodDef is unprovable inside a claim group, and a
    /// same-compiled-name group can legitimately hold a curried val beside a
    /// tupled one, so a disagreeing group under-sets to "unknown" rather than
    /// handing one method the other's grouping.
    arg_group_count: Option<usize>,
    /// [`ModuleMemberVal::curried`], a group fact for the same reason.
    curried: Option<CurriedArgs>,
    /// The val's doc-comment key. A group fact too, and the one that most needs
    /// to be: two documented overloads in one claim group carry two different
    /// keys, and handing a method its sibling's key would show the sibling's
    /// documentation on hover.
    xml_doc_sig: Option<String>,
    /// See [`ModuleMemberVal::typar_constraints`]; `None` for an augmentation.
    /// A group fact for the same reason the argument-group count is: a
    /// sibling's constraints are not this method's.
    typar_constraints: Option<Vec<FSharpConstraints>>,
    /// See [`ModuleMemberVal::val_facts`]; `Unknowable` for an augmentation.
    use_facts: crate::model::ValFacts,
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
    target_typars: Option<Option<usize>>,
    definition_range: Option<Option<FsharpSourceRange>>,
    arg_group_count: Option<Option<usize>>,
    curried: Option<Option<CurriedArgs>>,
    xml_doc_sig: Option<Option<String>>,
    typar_constraints: Option<Option<Vec<FSharpConstraints>>>,
    use_facts: Option<crate::model::ValFacts>,
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
/// - a `[<Literal>]` val claims the static field fsc emits for it (no
///   accessor MethodDef exists), by name;
/// - a non-literal val with no claimable member, and a projected member no
///   val claims, are both **recorded** on
///   [`skipped_members`](crate::Entity::skipped_members) — loud, bounded
///   uncertainty instead of a silent wrong member list.
fn rebuild_module_member_list(
    ecma: &mut Entity,
    target: &ModuleMemberTarget,
    physical: &PhysicalMethods,
) {
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
                if g.target_typars != Some(facts.target_typars) {
                    g.target_typars = None;
                }
                if g.definition_range.as_ref() != Some(&facts.definition_range) {
                    g.definition_range = None;
                }
                if g.arg_group_count.as_ref() != Some(&facts.arg_group_count) {
                    g.arg_group_count = None;
                }
                if g.curried.as_ref() != Some(&facts.curried) {
                    g.curried = None;
                }
                if g.xml_doc_sig.as_ref() != Some(&facts.xml_doc_sig) {
                    g.xml_doc_sig = None;
                }
                if g.typar_constraints.as_ref() != Some(&facts.typar_constraints) {
                    g.typar_constraints = None;
                }
                if g.use_facts.as_ref() != Some(&facts.use_facts) {
                    g.use_facts = None;
                }
            })
            .or_insert(GroupFacts {
                source_name: Some(facts.source_name),
                extension: Some(facts.instance_extension),
                fsharp_extension: Some(facts.fsharp_extension),
                target_typars: Some(facts.target_typars),
                definition_range: Some(facts.definition_range),
                arg_group_count: Some(facts.arg_group_count),
                curried: Some(facts.curried),
                xml_doc_sig: Some(facts.xml_doc_sig),
                typar_constraints: Some(facts.typar_constraints),
                use_facts: Some(facts.use_facts),
            });
    }

    let mut pool: Vec<Option<Member>> = std::mem::take(&mut ecma.members)
        .into_iter()
        .map(Some)
        .collect();
    // The names the member projection refused (recorded before this rebuild
    // adds its own skips), and per claim slot, how many vals claim it and how
    // many physical methods implement it — the doc key's ambiguity test,
    // counted physically because the projection can elide a val's own method
    // without a record (`[<CompilerGenerated>]`) and leave a hidden one.
    let refused: std::collections::HashSet<String> = ecma
        .skipped_members
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let mut vals_per_slot: HashMap<(&str, ClaimShape), usize> = HashMap::new();
    for v in target.vals.iter().filter(|v| !v.is_literal) {
        *vals_per_slot
            .entry((v.il_name(), claim_shape(v)))
            .or_default() += 1;
    }
    let slot_candidates: HashMap<(&str, ClaimShape), usize> = vals_per_slot
        .keys()
        .map(|&(name, shape)| {
            let n = ecma
                .method_def_tokens
                .iter()
                .filter_map(|t| physical.get(t))
                .filter(|m| physical_claim_slot_matches(m, name, shape))
                .count();
            ((name, shape), n)
        })
        .collect();
    let mut members = Vec::new();
    // Per slot whose vals carry different doc keys, the positions in `members`
    // of the methods they claimed, which are paired with the keys once every
    // val has claimed (see the key's stamp below).
    let mut unpaired: HashMap<(&str, ClaimShape), Vec<usize>> = HashMap::new();
    for v in &target.vals {
        // A `[<Literal>]` val compiles to a **static literal field**, not a method:
        // it claims that field. FCS brings it into scope (fsi: `open M` then bare
        // `LitVal` compiles), so a module without it would hide a bare name no
        // consumer could even know to be conservative about (see
        // `docs/assembly-module-open-plan.md`).
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
        let matches_slot = |slot: &Option<Member>| claim_slot_matches(slot, name, shape);
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
                // …and the count itself. The IL projector blanks every method of
                // an F# assembly to "unknown" (`Ecma335Assembly::enumerate_type_defs`)
                // because a flattened parameter list cannot distinguish a curried
                // `f a b` from a tupled `f (a, b)`. The pickle *can*: this is
                // FCS's own `ValReprInfo`, the same value it computes the split
                // from. Carrying it turns that unknown into the fact for every
                // val the merge claimed — which is what lets a consumer split a
                // parameterized active pattern's arguments, and lets the overload
                // engine commit where it previously had to defer.
                let facts = &groups[&(name, shape)];
                m.arg_group_count = facts.arg_group_count.flatten();
                // The val's type parameters' F#-only constraints, which have no
                // IL encoding, and whether its type mentions a unit of measure,
                // which IL erases — the two facts that decide whether the IL
                // signature may be read as the val's F# type. A reading list of
                // the wrong length means the val's quantifier and the method's
                // generic parameters do not correspond, so it is not applied.
                m.val_facts = facts
                    .use_facts
                    .clone()
                    .unwrap_or(crate::model::ValFacts::Unknowable);
                if let Some(Some(readings)) = &facts.typar_constraints
                    && readings.len() == m.generic_parameters.len()
                {
                    for (typar, reading) in m.generic_parameters.iter_mut().zip(readings) {
                        typar.fsharp_constraints = *reading;
                    }
                }
                // …and each group's arguments, by the same unanimity. A group
                // that does not account for the claimed method's parameters is
                // not this method's grouping, whatever claimed it.
                m.arg_groups = match facts.curried.clone().flatten() {
                    Some(c) if c.split(&m.signature.parameters).is_some() => {
                        ArgGroups::Read(Box::new(c))
                    }
                    _ => ArgGroups::Unknowable,
                };
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
                // The key fsc wrote for the val. Which val claimed which
                // MethodDef is unprovable inside a group, so the key is a group
                // fact: vals agreeing on one key hand it to each method they
                // claimed (two vals sharing one key is fsc's own collision, and
                // the doc XML holds one entry for both), while vals with keys of
                // their own are paired with the group's methods by signature
                // once every val has claimed (below).
                //
                // The claim itself is by name and arity, so the key must also
                // fit the claimed method's signature: a helper the signature
                // file hides can share the val's compiled name and arity and be
                // claimed in place of the val's own method.
                //
                // And the claim must be the only one available: when the module
                // holds more methods in the slot than vals claim it, the extra is
                // a member the signature hides, and which method is the val's is
                // a guess the key's parameter types cannot always settle (fsc's
                // array-rank spelling changed across versions). A refused method
                // of the slot's name vetoes it too.
                let unambiguous = slot_candidates.get(&(name, shape))
                    == vals_per_slot.get(&(name, shape))
                    && !refused.contains(name);
                m.xml_doc_sig = match &facts.xml_doc_sig {
                    Some(key) => key.clone().filter(|k| {
                        unambiguous
                            && crate::doc_id::recorded_method_key_fits(k, &m, v.measure_typars)
                    }),
                    None => {
                        if unambiguous {
                            unpaired
                                .entry((name, shape))
                                .or_default()
                                .push(members.len());
                        }
                        None
                    }
                };
                if facts.extension == Some(true) {
                    m.is_extension_method = true;
                }
                // The augmentation fact (instance *or* static) name resolution
                // hides on — authoritative here, since the val itself says so. A
                // conflicted group under-sets it like the others, and so the
                // target's arity.
                if facts.fsharp_extension == Some(true) {
                    m.augmentation = Augmentation::Certain {
                        target_typars: facts.target_typars.flatten(),
                    };
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
    // Pair each disagreeing group's keys with the methods its vals claimed,
    // when every val of the slot claimed one, every val is documented, and the
    // methods' signatures force which key is whose (`doc_id::pair_keys_by_fit`).
    for ((name, shape), claimed) in unpaired {
        let Some(keys) = target
            .vals
            .iter()
            .filter(|v| !v.is_literal && v.il_name() == name && claim_shape(v) == shape)
            .map(|v| v.xml_doc_sig.as_deref().map(|k| (k, v.measure_typars)))
            .collect::<Option<Vec<(&str, usize)>>>()
        else {
            continue;
        };
        let methods: Vec<&MethodLike> = claimed
            .iter()
            .map(|&i| match &members[i] {
                Member::Method(m) => &**m,
                _ => unreachable!("`unpaired` records claimed methods only"),
            })
            .collect();
        let Some(pairing) =
            crate::doc_id::pair_keys_by_fit(&keys, &methods, |&(key, erased), m| {
                crate::doc_id::recorded_method_key_fits(key, m, erased)
            })
        else {
            continue;
        };
        for (&i, k) in claimed.iter().zip(pairing) {
            if let Member::Method(m) = &mut members[i] {
                m.xml_doc_sig = Some(keys[k].0.to_string());
            }
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
    join: &PickleJoin,
    physical: &PhysicalMethods,
) -> Result<(), ImportError> {
    let rows = join.rows(entities);
    for target in collect_module_member_targets(pickled)? {
        let Some(ecma) = rows.entity_mut(entities, target.stamp) else {
            continue;
        };
        if !matches!(ecma.kind, EntityKind::Module) {
            continue;
        }
        rebuild_module_member_list(ecma, &target, physical);
    }
    Ok(())
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
/// that's the only shape with a merge target.
///
/// ## Which disagreements are fatal
///
/// The row is the one [`PickleJoin`] certifies as the measure's own. A measure
/// with **no row at all** in the image ([`JoinVerdict::NoRow`]), or whose own
/// row is not a class, is a disagreement between two successfully read sources
/// and fails the parse (D6.5). A measure whose row the join cannot attribute —
/// another row or another pickled entity at its path — or whose row the
/// projector dropped is not a disagreement but an absence, and is left alone.
pub(crate) fn merge_measure_entities(
    entities: &mut [Entity],
    pickled: &PickledCcu,
    join: &PickleJoin,
) -> Result<(), ImportError> {
    let mut measures: Vec<(u32, String)> = Vec::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |stamp, entity, _is_root, namespace, type_chain| {
            // Record every entity that is *both* `typar_kind == Measure` AND
            // carries the standalone-form repr (`FSharpObjectModel`), which
            // confirms a backing ECMA TypeDef exists — a measure abbreviation /
            // `MeasureAnnotatedAbbreviation` has `Measure` kind but no TypeDef,
            // so it is skipped. The synthetic root is `typar_kind == Type` and
            // filtered out by the kind check.
            let entity_has_ecma_typedef =
                matches!(entity.repr, PickledTyconRepr::FSharpObjectModel(_));
            if entity.typar_kind == TyparKind::Measure && entity_has_ecma_typedef {
                let mut segments = namespace.to_vec();
                segments.extend(type_chain.iter().cloned());
                segments.push(clr_name(entity));
                measures.push((stamp, segments.join(".")));
            }
            Ok(())
        },
    )?;

    let rows = join.rows(entities);
    for (stamp, display) in &measures {
        if join.verdict(*stamp) == Some(JoinVerdict::NoRow) {
            return Err(ImportError::FsharpPickleMergeMismatch {
                detail: format!("pickled measure entity {display} has no matching ECMA TypeDef"),
            });
        }
        let Some(ecma) = rows.entity_mut(entities, *stamp) else {
            continue;
        };
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
                        "pickled measure entity {display} has ECMA kind {other:?}, expected Class"
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
    join: &PickleJoin,
) -> Result<(), ImportError> {
    merge_measure_entities(entities, pickled, join)
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
/// `<PrivateImplementationDetails>`, …), and rows [`PickleJoin`] cannot
/// attribute to a pickled entity, keep their relative metadata order after the
/// pickled ones (stable sort with an infinite rank).
pub(crate) fn apply_declaration_order(
    entities: &mut [Entity],
    pickled: &PickledCcu,
    join: &PickleJoin,
) -> Result<(), ImportError> {
    // Each joined row's position in a pre-order walk of the pickle. Siblings
    // are visited in declaration order, so within any one list of sibling rows
    // the ranks are in declaration order too.
    let mut rank: HashMap<RowPath, usize> = HashMap::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |stamp, _entity, _is_root, _namespace, _type_chain| {
            if let Some(row) = join.joined_path(stamp) {
                let next = rank.len();
                rank.insert(row.clone(), next);
            }
            Ok(())
        },
    )?;
    fn reorder(rows: &mut [Entity], paths: Vec<Option<RowPath>>, rank: &HashMap<RowPath, usize>) {
        let ranks: Vec<usize> = paths
            .iter()
            .map(|p| {
                p.as_ref()
                    .and_then(|p| rank.get(p))
                    .copied()
                    .unwrap_or(usize::MAX)
            })
            .collect();
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by_key(|&i| ranks[i]);
        apply_permutation(rows, &order);
        for (row, &i) in rows.iter_mut().zip(&order) {
            let children = row
                .nested_types
                .iter()
                .map(|child| paths[i].as_ref().and_then(|p| p.child(child)))
                .collect();
            reorder(&mut row.nested_types, children, rank);
        }
    }
    let top = entities.iter().map(RowPath::of_top_level).collect();
    reorder(entities, top, &rank);
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
/// The pickled attribute whose class is `path`, if the declaration carries one.
///
/// Keyed exactly like [`has_auto_open_attribute`]'s test — by the non-local
/// entity reference's path — because that is the only spelling of an attribute
/// class the pickle holds. A `Local` tcref is an attribute *declared in this
/// same assembly*, which neither of the classes read here ever is.
fn pickled_attribute<'a>(
    pickled: &PickledCcu,
    attribs: &'a [PickledAttribute],
    path: &[&str],
) -> Option<&'a PickledAttribute> {
    attribs.iter().find(|a| match &a.tcref {
        PickledTcRef::NonLocal(idx) => {
            pickled
                .header
                .nlerefs
                .get(*idx as usize)
                .is_some_and(|nle| {
                    let actual: Vec<&str> = nle
                        .path
                        .iter()
                        .filter_map(|&i| pickled.header.strings.get(i as usize).map(String::as_str))
                        .collect();
                    actual == path
                })
        }
        PickledTcRef::Local(_) => false,
    })
}

/// The constant an attribute argument evaluates to, seeing through the
/// pass-through `Coerce` wrapper an `obj`-typed parameter introduces.
///
/// `None` for every other expression shape. An attribute argument that is not a
/// literal cannot be read here, and the callers treat that as *undecoded*
/// rather than as absent — see [`pickled_obsolete`].
fn pickled_attribute_const(expr: &PickledAttribExpr) -> Option<&PickledConst> {
    fn strip(e: &PickledExpr) -> Option<&PickledConst> {
        match e {
            PickledExpr::Const { value, .. } => Some(value),
            PickledExpr::Coerce { arg } => strip(arg),
            _ => None,
        }
    }
    // `evaluated` is FCS's constant-folded form, which is what an attribute
    // argument's *value* is; `orig` keeps the source spelling.
    strip(&expr.evaluated).or_else(|| strip(&expr.orig))
}

/// `[<System.Obsolete>]` as carried by the pickle.
///
/// `is_error` is the second positional argument (`ObsoleteAttribute` exposes it
/// through constructors only — the property is get-only, so there is no named
/// form to read). An argument list we cannot read yields `is_error: true`: the
/// attribute is *present*, so the only question is whether F# rejects a use,
/// and answering "no" on an argument we failed to decode is the one answer that
/// can produce a wrong commit. Over-rejecting costs a decline, which the
/// certain-implies-exact contract always permits.
fn pickled_obsolete(pickled: &PickledCcu, attribs: &[PickledAttribute]) -> Option<Obsolete> {
    let attr = pickled_attribute(pickled, attribs, &["System", "ObsoleteAttribute"])?;
    let message = match attr.args_unnamed.first().map(pickled_attribute_const) {
        // `[<Obsolete>]` — the nullary constructor carries no message.
        None => None,
        Some(Some(PickledConst::String(s))) => Some(s.clone()),
        Some(_) => {
            return Some(Obsolete {
                message: None,
                is_error: true,
            });
        }
    };
    let is_error = match attr.args_unnamed.get(1).map(pickled_attribute_const) {
        None => false,
        Some(Some(PickledConst::Bool(b))) => *b,
        Some(_) => true,
    };
    Some(Obsolete { message, is_error })
}

/// `[<CompilerMessage>]` as carried by the pickle — F#'s own error-level
/// diagnostic marker, which `CheckEntityAttributes` consults on the F#-tycon
/// path.
///
/// The `marker` number is read because it is load-bearing rather than
/// informational (see [`CompilerMessage::marker`]), and an unreadable one is
/// left `None` — that spelling makes a consumer treat the message as ordinary
/// rather than as the `nameof` marker, which is the conservative direction.
/// `IsError` is a named property here, unlike [`pickled_obsolete`]'s positional
/// flag, and an unreadable one rejects for the same reason.
fn pickled_compiler_message(
    pickled: &PickledCcu,
    attribs: &[PickledAttribute],
) -> Option<CompilerMessage> {
    let attr = pickled_attribute(
        pickled,
        attribs,
        &["Microsoft", "FSharp", "Core", "CompilerMessageAttribute"],
    )?;
    Some(decode_compiler_message(attr))
}

/// A pickled `[<CompilerMessage>]`'s payload — see [`pickled_compiler_message`]
/// for how each part is read.
fn decode_compiler_message(attr: &PickledAttribute) -> CompilerMessage {
    let message = match attr.args_unnamed.first().map(pickled_attribute_const) {
        Some(Some(PickledConst::String(s))) => Some(s.clone()),
        _ => None,
    };
    let marker = match attr.args_unnamed.get(1).map(pickled_attribute_const) {
        Some(Some(PickledConst::Int32(n))) => Some(*n),
        _ => None,
    };
    let is_error = match attr.args_named.iter().find(|a| a.name == "IsError") {
        None => false,
        Some(arg) => match pickled_attribute_const(&arg.value) {
            Some(PickledConst::Bool(b)) => *b,
            _ => true,
        },
    };
    CompilerMessage {
        message,
        marker,
        is_error,
    }
}

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
    /// The abbreviation's stamp, by which [`PickleJoin`] finds the container
    /// its marker hangs in: its namespace, or its enclosing module's row.
    stamp: u32,
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
    /// The pickled `[<Obsolete>]` and `[<CompilerMessage>]` attributes, the two
    /// `CheckEntityAttributes` consults that can make F# **reject** a use of the
    /// name outright (`FS0101` / `FS12004`).
    ///
    /// An abbreviation emits no ECMA TypeDef, so no projected row carries them
    /// and this marker is their only carrier. Reading them here is what lets
    /// `None` on the synthesised entity mean *absent* rather than *not looked
    /// at* — a distinction a consumer deciding "may I publish this type?" reads
    /// as the difference between a sound commit and a wrong one.
    obsolete: Option<Obsolete>,
    compiler_message: Option<CompilerMessage>,
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
/// The markers carry **no members and no base type** — only the name, the
/// pickled typar names (as `generic_parameters`, so arity-keyed lookups treat
/// them faithfully), and the decoded **logical target**
/// ([`Entity::abbreviation_target`], `None` for a shape the decoder cannot
/// faithfully model). A consumer recognises a marker by kind and either
/// resolves *through* the target (sema's `resolve_abbreviation_target`) or treats the
/// hit as "this name is taken by an abbreviation whose target we cannot
/// see" — a defer signal. FSharp.Core is not special: its primitive aliases
/// (`type int = int32`, `type 'T option = Option<'T>`, …) are ordinary
/// pickled abbreviations and get markers like any other assembly's, which is
/// how `int`/`option` resolve at all (they have no ECMA TypeDef).
///
/// Exclusions, each load-bearing:
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
    join: &PickleJoin,
    assembly: &AssemblyIdentity,
) -> Result<(), ImportError> {
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
        &mut |stamp, entity, is_root, _namespace, _type_chain| {
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
                    stamp,
                    name: clr_name(entity),
                    source_name: None,
                    typar_names: Vec::new(),
                    is_exception: true,
                    is_auto_open: false,
                    obsolete: pickled_obsolete(pickled, &entity.attribs),
                    compiler_message: pickled_compiler_message(pickled, &entity.attribs),
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
                stamp,
                name: clr_name(entity),
                source_name,
                typar_names,
                is_exception: false,
                is_auto_open: has_auto_open_attribute(pickled, &entity.attribs),
                obsolete: pickled_obsolete(pickled, &entity.attribs),
                compiler_message: pickled_compiler_message(pickled, &entity.attribs),
                abbreviation_target,
                definition_range: resolve_entity_range(pickled, entity),
            });
            Ok(())
        },
    )?;
    let rows = join.rows(entities);
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
        // The container is the abbreviation's namespace, or the row of its
        // enclosing module as [`PickleJoin`] certifies it; a module row the
        // join cannot reach has nowhere to hang the marker.
        let Some(container) = rows.container_rows_mut(entities, target.stamp) else {
            continue;
        };
        if !container
            .rows
            .iter()
            .any(|e| container.contains(e) && same_key(e))
        {
            // A nested entity carries an empty namespace of its own — the
            // path lives on the outermost type.
            let namespace = container.namespace.clone().unwrap_or_default();
            container
                .rows
                .push(abbreviation_marker(assembly, namespace, target));
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
                // The reading `apply_typar_constraints` refines when the
                // pickle describes this abbreviation, which is every case but
                // a measure-bearing one. An alias may constrain its own
                // parameters (`type Sorted<'T when 'T : comparison> = 'T list`),
                // so a consumer chasing the alias to its target still has to
                // answer for the alias's own constraints — the marker being
                // name-only does not make that question go away.
                fsharp_constraints: FSharpConstraints::Unknowable,
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
        metadata_name: None,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        is_no_pia_local: false,
        obsolete: target.obsolete.clone(),
        compiler_message: target.compiler_message.clone(),
        experimental: None,
        default_member: None,
        compiler_feature_required: Vec::new(),
        source_name: target.source_name.clone(),
        // A name-only abbreviation marker is not a module, so it declares no
        // extension members.
        extension_member_names: Vec::new(),
        union_cases: UnionCases::Unknowable,
        union_case_makers: Vec::new(),
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
        // A marker has no members of its own; a consumer reaching one chases
        // its target, which carries its own reading.
        member_measures: MemberMeasures::Unknowable,
    }
}

/// The CLR TypeDef name for a pickled module/type entity, **arity-stripped**
/// as the projector strips [`Entity::name`]. FCS's `Entity.CompiledName` is
/// `entity_compiled_name` when set, else `LogicalName` (so a
/// `[<CompiledName("Metre")>] [<Measure>] type m` is `Metre`, not `m`), and a
/// *generic* entity's carries the arity suffix (`FSharpChoice`2`), which this
/// strips to `FSharpChoice`. The `"Module"` suffix of a
/// `[<CompilationRepresentation(ModuleSuffix)>]` module lives in
/// `logical_name` and is not an arity, so it survives the strip.
///
/// A name for display and for the name-keyed readings (an abbreviation
/// marker's name, a local tycon's logical path) — never a row's identity, which
/// strips alike for distinct rows. A row is reached through [`PickleJoin`].
fn clr_name(entity: &PickledEntity) -> String {
    let raw = entity
        .compiled_name
        .clone()
        .unwrap_or_else(|| entity.logical_name.clone());
    strip_arity(&raw).to_string()
}

/// Whether a pickled type has **no erased `[<Measure>]` type parameter**. A
/// measure typar carries no CLR metadata, so a measure-parameterised type's
/// pickled typars outnumber its row's generic parameters, and position `i`
/// names different typars on the two sides. An overlay that lines the two up
/// by position (constraints, case fields, member slots) declines such a type —
/// it keeps what it had, unknowable, the safe direction. A measure-FREE type's
/// typars and its row's generic parameters correspond one to one.
///
/// FCS `TyparFlags.Kind` (`TypedTree.fs`): a typar is a measure iff
/// `flags &&& 0b00001000100000000 == 0b00000000100000000`. An unknown typar
/// index is treated as a measure — declining (the safe direction) rather than
/// trusting a correspondence we cannot validate.
fn is_measure_free(pickled: &PickledCcu, entity: &PickledEntity) -> bool {
    entity.typars.iter().all(|&ti| {
        pickled
            .tables
            .typars
            .get(ti as usize)
            .is_some_and(|t| !is_measure_typar(t.flags))
    })
}

/// FCS `TyparFlags.Kind` (`TypedTree.fs`): a typar is a measure iff
/// `flags &&& 0b00001000100000000 == 0b00000000100000000`.
fn is_measure_typar(flags: i64) -> bool {
    const TYPAR_KIND_MASK: i64 = 0b00001000100000000;
    const TYPAR_KIND_MEASURE: i64 = 0b00000000100000000;
    flags & TYPAR_KIND_MASK == TYPAR_KIND_MEASURE
}

// ---------------------------------------------------------------------------
// Doc-comment keys of type members
// ---------------------------------------------------------------------------

/// Which projected member of a type one pickled doc key belongs to, as far as
/// the IL can tell: the slot a val compiles to (a method, or one accessor of a
/// property, by name, arity and staticness), or a `val` field the IL exposes
/// as a property.
///
/// The arity and staticness are the ones F# **declares** — what the projected
/// member has once [`settle_compiled_shapes`] has given it its declared shape
/// — and `compiled` says how the IL member fsc emitted differs from them
/// ([`DocSlot::compiled_shape`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DocSlot {
    Method {
        name: String,
        arity: usize,
        is_static: bool,
        compiled: Compiled,
    },
    /// A property's getter. `name` is the IL property's (a `[<CompiledName>]`
    /// renames it), `accessor` the getter method's — which keeps the val's
    /// own name (`member A` compiled as `B` has property `B`, getter `get_A`).
    Getter {
        name: String,
        accessor: String,
        index_arity: usize,
        is_static: bool,
        compiled: Compiled,
    },
    /// A property's setter; see [`DocSlot::Getter`].
    Setter {
        name: String,
        accessor: String,
        index_arity: usize,
        is_static: bool,
        compiled: Compiled,
    },
    ValField(String),
}

/// How fsc compiled a type's member val against the staticness F# declares
/// for it — the two disagree exactly where `MemberIsCompiledAsInstance`
/// (`TypedTreeOps.Attributes.fs`) overrides the declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Compiled {
    /// The IL member has the declared staticness and arity.
    AsDeclared,
    /// An instance member compiled as an IL static whose first parameter is
    /// the receiver (by reference, for a struct): every instance member of a
    /// null-as-true union ([`compiles_instance_members_as_static`]), and one
    /// carrying `[<CompilationRepresentation(Static)>]`, bar abstract slots,
    /// overrides and interface implementations, which stay instance.
    StaticWithReceiver,
    /// A static member carrying `[<CompilationRepresentation(Instance)>]`,
    /// which fsc compiles as an IL instance method one parameter short — its
    /// first argument has become the IL `this`. No projected member can show
    /// the declared shape, so [`settle_compiled_shapes`] refuses it.
    InstanceFromStatic,
}

impl DocSlot {
    /// The slot as the IL member fsc emitted occupies it: the declared slot
    /// for [`Compiled::AsDeclared`] (and for a `val` field); otherwise a static
    /// one argument wider, or an instance one argument narrower (none narrower
    /// than nullary: the slot of a static compiled instance is refused
    /// whatever it holds, so it need only find what is there).
    fn compiled_shape(&self) -> DocSlot {
        let reshape = |arity: usize, compiled: Compiled| match compiled {
            Compiled::AsDeclared => (arity, None),
            Compiled::StaticWithReceiver => (arity + 1, Some(true)),
            Compiled::InstanceFromStatic => (arity.saturating_sub(1), Some(false)),
        };
        match self {
            DocSlot::Method {
                name,
                arity,
                is_static,
                compiled,
            } => {
                let (arity, il_static) = reshape(*arity, *compiled);
                DocSlot::Method {
                    name: name.clone(),
                    arity,
                    is_static: il_static.unwrap_or(*is_static),
                    compiled: Compiled::AsDeclared,
                }
            }
            DocSlot::Getter {
                name,
                accessor,
                index_arity,
                is_static,
                compiled,
            } => {
                let (index_arity, il_static) = reshape(*index_arity, *compiled);
                DocSlot::Getter {
                    name: name.clone(),
                    accessor: accessor.clone(),
                    index_arity,
                    is_static: il_static.unwrap_or(*is_static),
                    compiled: Compiled::AsDeclared,
                }
            }
            DocSlot::Setter {
                name,
                accessor,
                index_arity,
                is_static,
                compiled,
            } => {
                let (index_arity, il_static) = reshape(*index_arity, *compiled);
                DocSlot::Setter {
                    name: name.clone(),
                    accessor: accessor.clone(),
                    index_arity,
                    is_static: il_static.unwrap_or(*is_static),
                    compiled: Compiled::AsDeclared,
                }
            }
            DocSlot::ValField(name) => DocSlot::ValField(name.clone()),
        }
    }

    fn compiled(&self) -> Compiled {
        match self {
            DocSlot::Method { compiled, .. }
            | DocSlot::Getter { compiled, .. }
            | DocSlot::Setter { compiled, .. } => *compiled,
            DocSlot::ValField(_) => Compiled::AsDeclared,
        }
    }
}

/// The [`DocSlot`] a type's member val compiles to, or `None` for one with no
/// slot this overlay stamps: a class constructor, an F#-native extension (it
/// compiles onto its module, which the module member list keys), a val with no
/// `ValReprInfo` to count arguments from. A `[<CompiledName>]`-renamed val's
/// slot is named by the attribute: its pickled names are its logical ones,
/// which would name a *different* IL member (`member A` compiled as `B`
/// beside `member B` compiled as `D`).
///
/// An instance member's first argument group is the receiver, which the
/// declared signature does not list; a setter's last argument is the assigned
/// value, which a property's index parameters do not.
///
/// `instance_members_static` is the val's parent's
/// [`compiles_instance_members_as_static`].
fn member_doc_slot(
    pickled: &PickledCcu,
    fslib: &FslibAttributes<'_>,
    v: &PickledVal,
    instance_members_static: bool,
) -> Option<DocSlot> {
    let info = v.member_info.as_ref()?;
    if v.flags & VAL_FLAGS_IS_EXTENSION_MEMBER != 0 {
        return None;
    }
    let compiled = member_compiled(fslib, v, info, instance_members_static);
    // A `[<CompiledName>]` on a type member renames its IL member (an accessor's
    // property, too) but not the val's pickled names, so the slot is named by
    // the attribute. One whose argument cannot be read declines.
    let renamed = match pickled_attribute(
        pickled,
        &v.attribs,
        &["Microsoft", "FSharp", "Core", "CompiledNameAttribute"],
    ) {
        None => None,
        Some(attr) => match attr.args_unnamed.first().map(pickled_attribute_const) {
            Some(Some(PickledConst::String(name))) => Some(name.as_str()),
            _ => return None,
        },
    };
    let total: usize = v.repr_info.as_ref()?.arg_repr.iter().map(Vec::len).sum();
    let is_static = !info.flags.is_instance;
    let args = if is_static {
        total
    } else {
        total.checked_sub(1)?
    };
    let il_name = renamed
        .or(v.compiled_name.as_deref())
        .unwrap_or(&v.logical_name);
    Some(match info.flags.kind {
        PickledMemberKind::Member => DocSlot::Method {
            name: il_name.to_string(),
            arity: args,
            is_static,
            compiled,
        },
        PickledMemberKind::Constructor => DocSlot::Method {
            name: ".ctor".to_string(),
            arity: total,
            is_static: false,
            compiled: Compiled::AsDeclared,
        },
        PickledMemberKind::PropertyGet => DocSlot::Getter {
            name: renamed.or(v.logical_name.strip_prefix("get_"))?.to_string(),
            accessor: v
                .compiled_name
                .clone()
                .unwrap_or_else(|| v.logical_name.clone()),
            index_arity: args,
            is_static,
            compiled,
        },
        PickledMemberKind::PropertySet => DocSlot::Setter {
            name: renamed.or(v.logical_name.strip_prefix("set_"))?.to_string(),
            accessor: v
                .compiled_name
                .clone()
                .unwrap_or_else(|| v.logical_name.clone()),
            index_arity: args.checked_sub(1)?,
            is_static,
            compiled,
        },
        PickledMemberKind::ClassConstructor => return None,
    })
}

/// FCS's `MemberIsCompiledAsInstance` for a type's intrinsic member val,
/// reported against the staticness the val declares.
///
/// An abstract slot, an override or an interface implementation compiles as
/// it is declared. Any other member compiles instance when it carries
/// `[<CompilationRepresentation(Instance)>]`, or when it is declared instance,
/// carries no `Static` flag, and its parent does not compile instance members
/// as statics.
fn member_compiled(
    fslib: &FslibAttributes<'_>,
    v: &PickledVal,
    info: &crate::fsharp_pickle::model::PickledMemberInfo,
    instance_members_static: bool,
) -> Compiled {
    if matches!(
        info.flags.kind,
        PickledMemberKind::Constructor | PickledMemberKind::ClassConstructor
    ) {
        return Compiled::AsDeclared;
    }
    let declared_instance = info.flags.is_instance;
    let compiled_instance = if info.flags.is_dispatch_slot
        || info.flags.is_override_or_explicit_impl
        || !info.implemented_slots.is_empty()
    {
        declared_instance
    } else {
        let flags = fslib.compilation_representation(&v.attribs);
        flags & COMPILATION_REPRESENTATION_INSTANCE != 0
            || (declared_instance
                && flags & COMPILATION_REPRESENTATION_STATIC == 0
                && !instance_members_static)
    };
    match (declared_instance, compiled_instance) {
        (true, false) => Compiled::StaticWithReceiver,
        (false, true) => Compiled::InstanceFromStatic,
        _ => Compiled::AsDeclared,
    }
}

/// The `CompilationRepresentationFlags` bits `MemberIsCompiledAsInstance` and
/// `IsUnionTypeWithNullAsTrueValue` read (`prim-types.fs`).
const COMPILATION_REPRESENTATION_STATIC: i32 = 0x01;
const COMPILATION_REPRESENTATION_INSTANCE: i32 = 0x02;
const COMPILATION_REPRESENTATION_USE_NULL_AS_TRUE_VALUE: i32 = 0x08;

/// Recognises an attribute as one of FSharp.Core's own classes the way FCS's
/// `resolveAttribPath` does: a reference into the FSharp.Core CCU by its path,
/// or — only in FSharp.Core's own pickle, where those classes are local — a
/// local type at that path. A same-named class anywhere else is not the
/// well-known attribute, and FCS does not treat it as one.
struct FslibAttributes<'a> {
    pickled: &'a PickledCcu,
    /// Every local type's logical path, when the host CCU is FSharp.Core.
    local_paths: Option<HashMap<u32, Vec<String>>>,
}

impl<'a> FslibAttributes<'a> {
    const FSLIB: &'static str = "FSharp.Core";

    fn new(pickled: &'a PickledCcu) -> Result<Self, ImportError> {
        let is_fslib = pickled
            .tables
            .tycons
            .get(pickled.root_entity as usize)
            .is_some_and(|root| root.logical_name == Self::FSLIB);
        Ok(Self {
            pickled,
            local_paths: if is_fslib {
                Some(local_tycon_fqns(pickled)?)
            } else {
                None
            },
        })
    }

    /// The first attribute in `attribs` whose class is
    /// `Microsoft.FSharp.Core.<name>`.
    fn find<'b>(
        &self,
        attribs: &'b [PickledAttribute],
        name: &str,
    ) -> Option<&'b PickledAttribute> {
        let path = ["Microsoft", "FSharp", "Core", name];
        let header = &self.pickled.header;
        attribs.iter().find(|a| match &a.tcref {
            PickledTcRef::NonLocal(idx) => header.nlerefs.get(*idx as usize).is_some_and(|nle| {
                header
                    .ccu_refs
                    .get(nle.ccu as usize)
                    .is_some_and(|ccu| ccu.name == Self::FSLIB)
                    && nle.path.len() == path.len()
                    && nle.path.iter().zip(path).all(|(&i, want)| {
                        header.strings.get(i as usize).map(String::as_str) == Some(want)
                    })
            }),
            PickledTcRef::Local(stamp) => self
                .local_paths
                .as_ref()
                .and_then(|paths| paths.get(stamp))
                .is_some_and(|p| p.iter().map(String::as_str).eq(path)),
        })
    }

    /// The `[<CompilationRepresentation>]` flags `attribs` carry, decoded as
    /// FCS's `computeEntityWellKnownFlags` does: the single positional argument,
    /// constant-folded to an `int32`. Any other argument shape carries no flags
    /// there, and so none here.
    fn compilation_representation(&self, attribs: &[PickledAttribute]) -> i32 {
        let Some(attr) = self.find(attribs, "CompilationRepresentationAttribute") else {
            return 0;
        };
        match attr.args_unnamed.as_slice() {
            [arg] => match &arg.evaluated {
                PickledExpr::Const {
                    value: PickledConst::Int32(flags),
                    ..
                } => *flags,
                _ => 0,
            },
            _ => 0,
        }
    }
}

/// FCS's `TyconCompilesInstanceMembersAsStatic`, which is
/// `IsUnionTypeWithNullAsTrueValue`: a union with no cases, or one carrying
/// `[<CompilationRepresentation(UseNullAsTrueValue)>]` whose cases are exactly
/// one nullary case and at least one carrying case. fsc compiles such a
/// union's instance members as statics taking the receiver (FS0061), because
/// its nullary case is `null`, on which no instance method can be called.
///
/// A union whose representation a signature file hides pickles with no cases
/// to count (`NoRepr`). Its flag still decides: fsc rejects `UseNullAsTrueValue`
/// on anything but a union of that shape (FS1196), so a compiled type carrying
/// it is one.
fn compiles_instance_members_as_static(
    fslib: &FslibAttributes<'_>,
    entity: &PickledEntity,
) -> bool {
    let flagged = || {
        fslib.compilation_representation(&entity.attribs)
            & COMPILATION_REPRESENTATION_USE_NULL_AS_TRUE_VALUE
            != 0
    };
    let cases = match &entity.repr {
        PickledTyconRepr::Union(cases) => cases,
        PickledTyconRepr::UnionWithStaticFields { cases, .. } => cases,
        PickledTyconRepr::NoRepr => return flagged(),
        _ => return false,
    };
    cases.is_empty()
        || (flagged()
            && cases.iter().filter(|c| c.fields.is_empty()).count() == 1
            && cases.iter().any(|c| !c.fields.is_empty()))
}

/// Whether `member` occupies `slot`, by the slot's declared arity and
/// staticness; [`DocSlot::compiled_shape`] gives the slot the IL member
/// occupies before [`settle_compiled_shapes`] reshapes it.
fn occupies(member: &Member, slot: &DocSlot) -> bool {
    match (member, slot) {
        (
            Member::Method(m),
            DocSlot::Method {
                name,
                arity,
                is_static,
                ..
            },
        ) => m.name == *name && m.signature.parameters.len() == *arity && m.is_static == *is_static,
        (
            Member::Property(p),
            DocSlot::Getter {
                name,
                index_arity,
                is_static,
                ..
            },
        ) => {
            p.has_getter
                && p.name == *name
                && p.parameters.len() == *index_arity
                && p.is_static == *is_static
        }
        (
            Member::Property(p),
            DocSlot::Setter {
                name,
                index_arity,
                is_static,
                ..
            },
        ) => {
            p.has_setter
                && p.name == *name
                && p.parameters.len() == *index_arity
                && p.is_static == *is_static
        }
        // A `[<CLIEvent>]` property compiles to an IL event, not a property;
        // its getter val carries the event's key.
        (
            Member::Event(e),
            DocSlot::Getter {
                name,
                index_arity: 0,
                is_static,
                ..
            },
        ) => e.name == *name && e.is_static == *is_static,
        (Member::Property(p), DocSlot::ValField(name)) => {
            p.name == *name && p.parameters.is_empty() && !p.is_static
        }
        _ => false,
    }
}

/// One pickled F# type's member vals.
struct TypeMemberTarget {
    /// The type's stamp, by which [`PickleJoin`] finds its row.
    stamp: u32,
    /// The type's namespace, arity-stripped CLR type chain and pickled arity:
    /// how a member signature spells the type itself when it names it
    /// ([`leads_with_receiver`]).
    namespace: Vec<String>,
    type_chain: Vec<String>,
    arity: usize,
    /// Each member val's (or `val` field's) slot and pickled key, in pickle
    /// order. A slot can repeat: same-name, same-arity overloads share one.
    keys: Vec<DocKey>,
}

/// One pickled member val's (or `val` field's) doc key and where it compiles.
#[derive(Debug, Clone)]
struct DocKey {
    slot: DocSlot,
    /// `None` for an undocumented val.
    key: Option<String>,
    /// The val's measure type parameters, which IL erases but its key's generic
    /// arity counts (see `doc_id::recorded_key_fits`).
    erased_typars: usize,
    /// For a method or constructor val, its argument groups as a caller writes
    /// them; `None` for any other val, and for one without a `ValReprInfo`.
    curried: Option<CurriedArgs>,
    /// Whether the val is an abstract slot or an override, which F# pickles as
    /// two vals for the one IL method a slot with its `default` compiles to.
    role: SlotRole,
}

/// A member val's part in virtual dispatch, as its `MemberFlags` record it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotRole {
    /// Neither an abstract slot nor an override.
    Plain,
    /// `abstract` (`IsDispatchSlot`).
    DispatchSlot,
    /// `default`, `override`, or an explicit interface implementation
    /// (`IsOverrideOrExplicitImpl`).
    Override,
}

/// Settle each F# type's members against its pickled member vals, in these
/// steps per type:
///
/// 1. [`settle_compiled_shapes`]: give a member fsc compiled under another
///    staticness than F# declares the shape F# declares (or refuse it), so the
///    steps below, and every consumer, see the member F# code calls.
/// 2. [`surface_case_maker_members`]: a union member whose own IL method fsc
///    discarded for the case maker of the same name.
/// 3. [`stamp_doc_sigs`]: the doc-comment key fsc pickled for each member
///    ([`crate::MethodLike::xml_doc_sig`], [`crate::Property::xml_doc_sig`] and
///    [`crate::Property::setter_xml_doc_sig`]) — the type-member counterpart of
///    the module member list's claim, which keys module members.
/// 4. [`stamp_arg_groups`]: each method's argument groups.
///
/// A type's member vals are its `tcaug.adhoc` entries (what FCS's
/// `MembersOfFSharpTyconSorted` reads); a class's or struct's `val` fields are
/// its object-model fields, keyed `F:` although an immutable one compiles to a
/// property. Each val is matched to the projected member occupying its
/// [`DocSlot`], and a slot is stamped only when that is unambiguous: exactly as
/// many members occupy the slot as vals claim it, and either every val sharing
/// the slot carries the one key, or each carries its own and the members'
/// signatures force which key is whose ([`crate::doc_id::pair_keys_by_fit`]).
/// Two documented `Add` overloads share the slot `Add/1`; `Add(int)` and
/// `Add(string)` each take the key that spells their parameter, while two
/// whose keys cannot be told apart by signature are not stamped — handing a
/// method its sibling's key would show the sibling's documentation — and keep
/// their computed IDs.
///
/// The type's row is the one [`PickleJoin`] certifies as its own. A type with
/// an erased `[<Measure>]` parameter is skipped: its member vals' slots are
/// matched by arity, and IL counts no erased parameter. Skipping costs coverage
/// only — a member left unstamped keeps its computed ID, and one left unshaped
/// keeps its IL shape.
pub(crate) fn apply_type_member_overlays(
    entities: &mut [Entity],
    pickled: &PickledCcu,
    join: &PickleJoin,
    physical: &PhysicalMethods,
) -> Result<(), ImportError> {
    let rows = join.rows(entities);
    for target in type_member_targets(pickled)? {
        if target.keys.is_empty() {
            continue;
        }
        // The type's own row, as the join certifies it; a pickled type never
        // compiles to a module's row.
        let Some(ecma) = rows
            .entity_mut(entities, target.stamp)
            .filter(|e| e.kind != EntityKind::Module)
        else {
            continue;
        };
        overlay_type_row(ecma, &target, physical);
    }
    Ok(())
}

/// The steps of [`apply_type_member_overlays`] on one located row.
fn overlay_type_row(ecma: &mut Entity, target: &TypeMemberTarget, physical: &PhysicalMethods) {
    let mut reshaped = settle_compiled_shapes(ecma, target, physical);
    surface_case_maker_members(ecma, &target.keys, physical, &reshaped);
    reshaped.resize(ecma.members.len(), false);
    stamp_doc_sigs(ecma, &target.keys, physical, &reshaped);
    stamp_arg_groups(ecma, &target.keys, physical, &reshaped);
}

/// Whether the `i`th member of a row holds `slot`: it occupies the slot's
/// declared shape, *and* it came by that shape the way the slot's val
/// compiled — a receiver-first val's slot holds only a member
/// [`settle_compiled_shapes`] reshaped (`reshaped[i]`), an as-declared val's
/// only one it did not, and a static-compiled-instance val's none (its member
/// was refused). Occupancy alone cannot tell a reshaped member from one
/// declared in that shape, so without the second half a receiver-first val
/// whose own method the projection elided would claim another val's member.
fn holds(reshaped: &[bool], i: usize, member: &Member, slot: &DocSlot) -> bool {
    occupies(member, slot)
        && match slot.compiled() {
            Compiled::AsDeclared => !reshaped[i],
            Compiled::StaticWithReceiver => reshaped[i],
            Compiled::InstanceFromStatic => false,
        }
}

/// Every pickled F# type (not a module, namespace, abbreviation or
/// measure-parameterised type) with its member vals' [`DocKey`]s.
fn type_member_targets(pickled: &PickledCcu) -> Result<Vec<TypeMemberTarget>, ImportError> {
    let fslib = FslibAttributes::new(pickled)?;
    // A type's member vals are declared in its *enclosing* module or
    // namespace's val list, each naming the type as its apparent parent (the
    // type's own `tcaug.adhoc` holds non-local references back to them). So one
    // walk collects every type's location, by stamp, and every member val, by
    // its parent's stamp; the vals' keys are built once their parent is known,
    // because how a val compiles depends on it.
    let mut targets: HashMap<u32, (TypeMemberTarget, bool)> = HashMap::new();
    let mut member_vals: HashMap<PickledTcRef, Vec<&PickledVal>> = HashMap::new();
    let mut parents: HashMap<u32, u32> = HashMap::new();
    let mut logical_paths: HashMap<u32, Vec<String>> = HashMap::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |stamp, entity, is_root, namespace, type_chain| {
            for &vi in &entity.module_type.vals {
                let v = pickled.tables.vals.get(vi as usize).ok_or(
                    ImportError::OsgnIndexOutOfRange {
                        kind: "val (type member doc keys)",
                        index: vi,
                        max: pickled.tables.vals.len(),
                    },
                )?;
                if let Some(info) = &v.member_info {
                    member_vals
                        .entry(info.apparent_parent.clone())
                        .or_default()
                        .push(v);
                }
            }
            // The entity's logical path from the CCU root, which is how a
            // non-local reference back into this CCU names it. A parent is
            // visited before its children, so its path is already recorded.
            let path = match parents.get(&stamp) {
                Some(parent) => {
                    let mut p = logical_paths.get(parent).cloned().unwrap_or_default();
                    p.push(entity.logical_name.clone());
                    p
                }
                None => Vec::new(),
            };
            for &child in &entity.module_type.entities {
                parents.insert(child, stamp);
            }
            logical_paths.insert(stamp, path);
            if is_root
                || entity.flags & ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE != 0
                || entity.type_abbrev.is_some()
                || !is_measure_free(pickled, entity)
            {
                return Ok(());
            }
            let mut keys = Vec::new();
            if let PickledTyconRepr::FSharpObjectModel(model) = &entity.repr {
                for field in model.rfields.iter().filter(|f| !f.is_static) {
                    keys.push(DocKey {
                        slot: DocSlot::ValField(field.ident.name.clone()),
                        key: pickled_xml_doc_sig(&field.xmldoc_sig),
                        erased_typars: 0,
                        curried: None,
                        role: SlotRole::Plain,
                    });
                }
            }
            let mut chain = type_chain.to_vec();
            chain.push(clr_name(entity));
            targets.insert(
                stamp,
                (
                    TypeMemberTarget {
                        stamp,
                        namespace: namespace.to_vec(),
                        type_chain: chain,
                        arity: entity.typars.len(),
                        keys,
                    },
                    compiles_instance_members_as_static(&fslib, entity),
                ),
            );
            Ok(())
        },
    )?;
    let host = pickled
        .tables
        .tycons
        .get(pickled.root_entity as usize)
        .map(|root| root.logical_name.as_str());
    let by_path: HashMap<&[String], u32> = logical_paths
        .iter()
        .map(|(stamp, path)| (path.as_slice(), *stamp))
        .collect();
    for (parent, vals) in member_vals {
        let stamp = match parent {
            PickledTcRef::Local(stamp) => Some(stamp),
            // fsc pickles a member's apparent parent as a path into the CCU —
            // its own CCU, for an intrinsic member.
            PickledTcRef::NonLocal(index) => pickled
                .header
                .nlerefs
                .get(index as usize)
                .filter(|nle| {
                    pickled
                        .header
                        .ccu_refs
                        .get(nle.ccu as usize)
                        .is_some_and(|ccu| Some(ccu.name.as_str()) == host)
                })
                .and_then(|nle| {
                    nle.path
                        .iter()
                        .map(|&i| pickled.header.strings.get(i as usize).cloned())
                        .collect::<Option<Vec<String>>>()
                })
                .and_then(|path| by_path.get(path.as_slice()).copied()),
        };
        // A parent the walk did not record as a type is a module (whose
        // members the member list keys), a type this overlay skips, or an
        // entity of another CCU.
        let Some((target, instance_members_static)) = stamp.and_then(|s| targets.get_mut(&s))
        else {
            continue;
        };
        target.keys.extend(
            vals.into_iter()
                .filter_map(|v| member_doc_key(pickled, &fslib, v, *instance_members_static)),
        );
    }
    Ok(targets.into_values().map(|(target, _)| target).collect())
}

/// The [`DocKey`] of a type's member val, or `None` for one with no
/// [`DocSlot`] (see [`member_doc_slot`]).
fn member_doc_key(
    pickled: &PickledCcu,
    fslib: &FslibAttributes<'_>,
    v: &PickledVal,
    instance_members_static: bool,
) -> Option<DocKey> {
    let info = v.member_info.as_ref()?;
    let slot = member_doc_slot(pickled, fslib, v, instance_members_static)?;
    Some(DocKey {
        slot,
        key: pickled_xml_doc_sig(&v.xmldoc_sig),
        erased_typars: measure_typar_count(v),
        curried: match info.flags.kind {
            PickledMemberKind::Member if info.flags.is_instance => {
                curried_args(v, ReceiverAt::This)
            }
            PickledMemberKind::Member | PickledMemberKind::Constructor => {
                curried_args(v, ReceiverAt::None)
            }
            PickledMemberKind::PropertyGet
            | PickledMemberKind::PropertySet
            | PickledMemberKind::ClassConstructor => None,
        },
        role: if info.flags.is_dispatch_slot {
            SlotRole::DispatchSlot
        } else if info.flags.is_override_or_explicit_impl {
            SlotRole::Override
        } else {
            SlotRole::Plain
        },
    })
}

/// Give each member fsc compiled under another staticness than F# declares
/// ([`Compiled`]) the shape F# declares, which is the shape FCS shows and F#
/// code calls: an instance member compiled as a static taking the receiver
/// becomes an instance member without the receiver parameter.
///
/// The vals are grouped by the IL slot they compile to
/// ([`DocSlot::compiled_shape`]), and a group's occupants are reshaped only
/// when that is unambiguous, the way a doc key is stamped: every val in the
/// group compiled with its receiver first, exactly as many projected members
/// and physical methods occupy the IL slot as vals claim it, and each
/// occupant's first parameter is this type (by reference, for a struct).
/// Anything else — an occupant that is not the receiver-first shape, a group
/// that also holds a val declared static (whose IL slot is the same), a count
/// that does not add up, or a static compiled as an instance method
/// ([`Compiled::InstanceFromStatic`]) — refuses every occupant of the IL slot,
/// recording it on [`Entity::skipped_members`]: in its IL shape it is a member
/// F# does not have, so it must be visibly absent rather than present and
/// wrong. A property that one accessor's group reshapes and another's refuses
/// is refused.
///
/// Returns, per member of the settled row, whether this reshaped it — what
/// [`holds`] needs to keep each reshaped member its own val's.
fn settle_compiled_shapes(
    entity: &mut Entity,
    target: &TypeMemberTarget,
    physical: &PhysicalMethods,
) -> Vec<bool> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Settle {
        Reshape,
        Refuse,
    }
    // Per IL slot: how many vals compile to it, and how many of those compiled
    // with their receiver first.
    let mut groups: HashMap<DocSlot, (usize, usize, bool)> = HashMap::new();
    for k in &target.keys {
        let entry = groups
            .entry(k.slot.compiled_shape())
            .or_insert((0, 0, false));
        entry.0 += 1;
        match k.slot.compiled() {
            Compiled::AsDeclared => {}
            Compiled::StaticWithReceiver => entry.1 += 1,
            Compiled::InstanceFromStatic => entry.2 = true,
        }
    }
    let mut decisions: HashMap<usize, Settle> = HashMap::new();
    for (il_slot, (vals, receiver_first, instance_from_static)) in &groups {
        if *receiver_first == 0 && !instance_from_static {
            continue;
        }
        let occupants: Vec<usize> = entity
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| occupies(m, il_slot))
            .map(|(i, _)| i)
            .collect();
        let unambiguous = !instance_from_static
            && receiver_first == vals
            && occupants.len() == *vals
            && physical_occupants(entity, il_slot, physical) == *vals;
        for i in occupants {
            let settle = if unambiguous && leads_with_receiver(&entity.members[i], target) {
                Settle::Reshape
            } else {
                Settle::Refuse
            };
            let entry = decisions.entry(i).or_insert(settle);
            if settle == Settle::Refuse {
                *entry = Settle::Refuse;
            }
        }
    }
    let mut refused = Vec::new();
    let mut reshaped = Vec::new();
    for (i, member) in std::mem::take(&mut entity.members).into_iter().enumerate() {
        match (decisions.get(&i), member) {
            (None, member) => {
                entity.members.push(member);
                reshaped.push(false);
            }
            (Some(Settle::Reshape), Member::Method(mut m)) => {
                m.signature.parameters.remove(0);
                m.is_static = false;
                entity.members.push(Member::Method(m));
                reshaped.push(true);
            }
            (Some(Settle::Reshape), Member::Property(mut p)) => {
                p.parameters.remove(0);
                p.is_static = false;
                entity.members.push(Member::Property(p));
                reshaped.push(true);
            }
            (Some(_), member) => refused.push(member),
        }
    }
    entity
        .skipped_members
        .extend(refused.into_iter().map(|member| {
            SkippedMember {
                name: match &member {
                    Member::Method(m) => m.name.clone(),
                    Member::Property(p) => p.name.clone(),
                    Member::Field(f) => f.name.clone(),
                    Member::Event(e) => e.name.clone(),
                },
                reason: "an F# member fsc compiled under another staticness than it declares, \
                     which the projection cannot attribute to its declaration"
                    .to_string(),
            }
        }));
    reshaped
}

/// Surface the case maker of a union as the static member method it stands
/// in for.
///
/// fsc names FSharp.Core's `Option` and `list` case makers after their cases
/// (`Some`, `Cons`) rather than `New<Case>`, and *discards* a member method of
/// the union that clashes with one by name (`IlxGen.fs`, `tdefDiscards`) —
/// `static member Some` and `static member Cons` among them — so the maker is
/// the IL method a call to the member binds to. The projection drops it as
/// compiler-generated, leaving the member absent.
///
/// The maker is surfaced for a val only when that is unambiguous: the val is
/// the only one claiming its static method slot, no projected member occupies
/// it, exactly one physical method does, and exactly one of the union's
/// [`Entity::union_case_makers`] is a method of that name and arity. The one
/// physical method is then the val's, and it is that maker.
fn surface_case_maker_members(
    entity: &mut Entity,
    keys: &[DocKey],
    physical: &PhysicalMethods,
    reshaped: &[bool],
) {
    if entity.kind != EntityKind::Union {
        return;
    }
    let mut claims: HashMap<&DocSlot, usize> = HashMap::new();
    for k in keys {
        *claims.entry(&k.slot).or_default() += 1;
    }
    let mut surfaced = Vec::new();
    for (slot, vals) in claims {
        let DocSlot::Method {
            name,
            arity,
            is_static: true,
            compiled: Compiled::AsDeclared,
        } = slot
        else {
            continue;
        };
        if vals != 1
            || entity
                .members
                .iter()
                .enumerate()
                .any(|(i, m)| holds(reshaped, i, m, slot))
            || physical_occupants(entity, slot, physical) != 1
        {
            continue;
        }
        let mut makers = entity.union_case_makers.iter().filter(|maker| {
            maker.method.name == *name
                && maker.method.is_static
                && maker.method.signature.parameters.len() == *arity
        });
        if let (Some(maker), None) = (makers.next(), makers.next()) {
            surfaced.push(maker.method.clone());
        }
    }
    // Metadata order, as the projector lists methods.
    surfaced.sort_by_key(|m| m.metadata_token);
    entity
        .members
        .extend(surfaced.into_iter().map(|m| Member::Method(Box::new(m))));
}

/// Whether `member`'s first IL parameter is the receiver fsc puts first when it
/// compiles an instance member of `target` as a static: `target` itself at its
/// own type parameters, by reference for a struct (only a method or a
/// property can lead with one).
fn leads_with_receiver(member: &Member, target: &TypeMemberTarget) -> bool {
    let is_self = |ty: &TypeRef| match ty {
        TypeRef::Named {
            assembly: None,
            namespace,
            name,
            type_args,
            ..
        } => {
            *namespace == target.namespace
                && name
                    .split('/')
                    .eq(target.type_chain.iter().map(String::as_str))
                && type_args.len() == target.arity
                && type_args.iter().enumerate().all(|(i, a)| {
                    a.ty == TypeRef::Var {
                        index: i as u16,
                        is_method: false,
                    }
                })
        }
        _ => false,
    };
    match member {
        Member::Method(m) => m
            .signature
            .parameters
            .first()
            .is_some_and(|p| is_self(&p.ty)),
        Member::Property(p) => p.parameters.first().is_some_and(|p| match &p.ty.ty {
            TypeRef::ByRef { inner, .. } => is_self(inner),
            ty => is_self(ty),
        }),
        Member::Field(_) | Member::Event(_) => false,
    }
}

/// Stamp `entity`'s methods with their vals' argument groups
/// ([`crate::MethodLike::arg_groups`]), from one type's pickled member vals.
///
/// A method slot is stamped only when that is unambiguous, as for the doc keys
/// ([`stamp_doc_sigs`]): every val in the slot groups its arguments alike, no
/// member of the slot's name was refused, and exactly as many methods occupy
/// the slot — projected and physical alike — as vals claim it. A member of a
/// type this pass does not reach stays [`ArgGroups::Unknowable`].
///
/// Several vals share a slot only as overloads of one name and arity, and F#
/// lets only a method of one tupled group be overloaded (FS0439), so their
/// groupings agree; the argument names need not (`O(a, b)` beside `O(x, y)`),
/// and which method is which val's the slot cannot tell. Such a slot is
/// stamped with the agreed grouping and each method's own IL parameter names,
/// which are its F# names but for a wildcard or a tuple-typed parameter's
/// elements (F# shows those unnamed; IL names them `_arg1`, `p_0`).
///
/// The count of methods the vals account for is not the count of vals: an
/// `abstract` slot and its `default` are two vals, and one IL method. Each
/// abstract val that an override in the slot can pair with is counted once,
/// which can only under-count the vals' methods — so an equal physical count
/// still proves every method of the slot is one of theirs.
fn stamp_arg_groups(
    entity: &mut Entity,
    keys: &[DocKey],
    physical: &PhysicalMethods,
    reshaped: &[bool],
) {
    /// A grouping without its names: the receiver flag, and each group's size.
    fn shape(c: &CurriedArgs) -> (bool, Vec<usize>) {
        (
            c.leading_receiver,
            c.groups.iter().map(|g| g.args.len()).collect(),
        )
    }
    /// Per method slot: how many vals claim it, their grouping if they all
    /// agree, and its shape if those agree.
    struct Slot<'a> {
        vals: usize,
        dispatch_slots: usize,
        overrides: usize,
        curried: Option<Option<&'a CurriedArgs>>,
        shape: Option<Option<(bool, Vec<usize>)>>,
    }
    let mut slots: HashMap<&DocSlot, Slot<'_>> = HashMap::new();
    for k in keys {
        if !matches!(k.slot, DocSlot::Method { .. }) {
            continue;
        }
        let curried = k.curried.as_ref();
        let shaped = curried.map(shape);
        let entry = slots.entry(&k.slot).or_insert_with(|| Slot {
            vals: 0,
            dispatch_slots: 0,
            overrides: 0,
            curried: Some(curried),
            shape: Some(shaped.clone()),
        });
        entry.vals += 1;
        match k.role {
            SlotRole::Plain => {}
            SlotRole::DispatchSlot => entry.dispatch_slots += 1,
            SlotRole::Override => entry.overrides += 1,
        }
        if entry.curried != Some(curried) {
            entry.curried = None;
        }
        if entry.shape.as_ref() != Some(&shaped) {
            entry.shape = None;
        }
    }
    for (slot, facts) in slots {
        let (Some(Some((leading_receiver, sizes))), DocSlot::Method { name, .. }) =
            (facts.shape, slot)
        else {
            continue;
        };
        if entity.skipped_members.iter().any(|s| s.name == *name) {
            continue;
        }
        // At most the methods the vals compile to: an abstract slot and its
        // default share one.
        let methods = facts.vals - facts.dispatch_slots.min(facts.overrides);
        let occupants = entity
            .members
            .iter()
            .enumerate()
            .filter(|(i, m)| holds(reshaped, *i, m, slot))
            .count();
        if occupants != methods || physical_occupants(entity, slot, physical) != methods {
            continue;
        }
        for (_, member) in entity
            .members
            .iter_mut()
            .enumerate()
            .filter(|(i, m)| holds(reshaped, *i, m, slot))
        {
            let Member::Method(m) = member else {
                continue;
            };
            let curried = match facts.curried {
                Some(Some(curried)) => curried.clone(),
                // The names disagree: each method's own, from its IL.
                _ => {
                    let mut names = m.signature.parameters.iter().map(|p| p.name.clone());
                    if leading_receiver {
                        names.next();
                    }
                    CurriedArgs {
                        leading_receiver,
                        groups: sizes
                            .iter()
                            .map(|&n| ArgGroup {
                                args: names
                                    .by_ref()
                                    .take(n)
                                    .map(|name| CurriedArg { name })
                                    .collect(),
                            })
                            .collect(),
                    }
                }
            };
            if curried.split(&m.signature.parameters).is_some() {
                m.arg_groups = ArgGroups::Read(Box::new(curried));
            }
        }
    }
}

/// One physical `MethodDef`'s IL slot, keyed by its metadata token: what the
/// doc-key stamp counts to decide a slot is unambiguous, including methods the
/// member projection refuses or deliberately elides — which the projected
/// member list cannot show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalMethod {
    pub(crate) name: String,
    /// `None` when the signature does not decode: the method may then occupy a
    /// slot of any arity.
    pub(crate) params: Option<usize>,
    pub(crate) is_static: bool,
    /// The IL property this method is the getter or setter of, if any. A
    /// `[<CompiledName>]` can make a getter's name lie about its property:
    /// two getters renamed to one name give one property backed by the
    /// first, and a second getter that backs nothing.
    pub(crate) accessor_of: Option<String>,
}

/// Every `MethodDef` of an image, by token.
pub(crate) type PhysicalMethods = HashMap<u32, PhysicalMethod>;

/// How many of `entity`'s physical methods implement `slot`, in the shape fsc
/// compiled it to ([`DocSlot::compiled_shape`]): a method by name, arity and
/// staticness; a getter or setter by its accessor method; a `val` field by its
/// getter. A `[<CLIEvent>]` property's getter slot is an IL event instead,
/// implemented by its `add_` accessor.
fn physical_occupants(entity: &Entity, slot: &DocSlot, physical: &PhysicalMethods) -> usize {
    let slot = &slot.compiled_shape();
    // The method's name, arity and staticness, and for an accessor the IL
    // property it must back.
    let (method, params, is_static, property) = match slot {
        DocSlot::Method {
            name,
            arity,
            is_static,
            ..
        } => (name.clone(), *arity, *is_static, None),
        DocSlot::Getter {
            name,
            accessor,
            index_arity,
            is_static,
            ..
        } => (accessor.clone(), *index_arity, *is_static, Some(name)),
        DocSlot::Setter {
            name,
            accessor,
            index_arity,
            is_static,
            ..
        } => (accessor.clone(), index_arity + 1, *is_static, Some(name)),
        DocSlot::ValField(name) => (format!("get_{name}"), 0, false, Some(name)),
    };
    let count = |name: &str, params: usize, property: Option<&String>| {
        entity
            .method_def_tokens
            .iter()
            .filter_map(|t| physical.get(t))
            .filter(|m| {
                m.name == name
                    && m.is_static == is_static
                    && m.params.is_none_or(|n| n == params)
                    && property.is_none_or(|p| m.accessor_of.as_ref() == Some(p))
            })
            .count()
    };
    let event_adders = match slot {
        DocSlot::Getter {
            name,
            index_arity: 0,
            ..
        } => count(&format!("add_{name}"), 1, None),
        _ => 0,
    };
    count(&method, params, property) + event_adders
}

/// Stamp `entity`'s members from one type's pickled keys; see
/// [`apply_type_member_overlays`] for when a slot is unambiguous.
fn stamp_doc_sigs(
    entity: &mut Entity,
    keys: &[DocKey],
    physical: &PhysicalMethods,
    reshaped: &[bool],
) {
    // Per slot: each claiming val's key and erased type parameters.
    let mut slots: HashMap<&DocSlot, Vec<(Option<&String>, usize)>> = HashMap::new();
    for k in keys {
        slots
            .entry(&k.slot)
            .or_default()
            .push((k.key.as_ref(), k.erased_typars));
    }
    // How many distinct slots reach each projected member's key, per key it
    // carries (a property's setter key is its own). A member two slots both
    // reach — one IL property that two `[<CompiledName>]`-renamed getters
    // share, only one of them its accessor — is no slot's provably, so
    // neither may stamp it.
    let is_setter = |slot: &DocSlot| matches!(slot, DocSlot::Setter { .. });
    let mut claims: HashMap<(usize, bool), usize> = HashMap::new();
    for slot in slots.keys() {
        for (i, m) in entity.members.iter().enumerate() {
            if holds(reshaped, i, m, slot) {
                *claims.entry((i, is_setter(slot))).or_default() += 1;
            }
        }
    }
    for (slot, facts) in slots {
        let vals = facts.len();
        // Undocumented vals stamp nothing, and a slot whose vals are not all
        // documented cannot be paired key by key.
        let Some(slot_keys) = facts
            .iter()
            .map(|&(key, erased)| key.map(|k| (k, erased)))
            .collect::<Option<Vec<(&String, usize)>>>()
        else {
            continue;
        };
        if entity.members.iter().enumerate().any(|(i, m)| {
            holds(reshaped, i, m, slot) && claims.get(&(i, is_setter(slot))).is_some_and(|&n| n > 1)
        }) {
            continue;
        }
        // A val's own member can be missing from the projection — refused and
        // recorded as skipped, or deliberately elided — while a member the
        // signature file hides sits in the same slot. The count below cannot
        // tell that member from the val's. A recorded skip under the slot's
        // name declines the slot outright; otherwise each occupant must fit
        // the key it is handed (`type_member_stamp_never_hands_a_member_another_vals_key`
        // pins both).
        let slot_name = match slot {
            DocSlot::Method { name, .. }
            | DocSlot::Getter { name, .. }
            | DocSlot::Setter { name, .. }
            | DocSlot::ValField(name) => name,
        };
        if entity.skipped_members.iter().any(|s| s.name == *slot_name)
            || entity.skipped_members.iter().any(|s| {
                s.name.strip_prefix("get_").or(s.name.strip_prefix("set_")) == Some(slot_name)
            })
        {
            continue;
        }
        let occupants: Vec<&Member> = entity
            .members
            .iter()
            .enumerate()
            .filter(|(i, m)| holds(reshaped, *i, m, slot))
            .map(|(_, m)| m)
            .collect();
        let keyed_as = match slot {
            DocSlot::Setter { .. } => crate::doc_id::KeyedAs::Setter,
            _ => crate::doc_id::KeyedAs::Member,
        };
        // Every method implementing the slot must be a claimed val's —
        // counted physically, since the member projection can elide the val's
        // own member without a record and leave a signature-hidden one in its
        // place.
        if occupants.len() != vals || physical_occupants(entity, slot, physical) != vals {
            continue;
        }
        // The key encodes the val's parameter types, so it refutes a member
        // that merely shares the slot (`doc_id::recorded_key_fits`). Vals that
        // agree on one key hand it to every occupant, each of which it must
        // fit; vals with keys of their own hand each occupant the one key its
        // signature forces (`doc_id::pair_keys_by_fit`).
        let fits = |&(key, erased): &(&String, usize), m: &&Member| {
            crate::doc_id::recorded_key_fits(key, m, keyed_as, erased)
        };
        let handed: Vec<&String> = if slot_keys.iter().all(|k| *k == slot_keys[0]) {
            if !occupants.iter().all(|m| fits(&slot_keys[0], m)) {
                continue;
            }
            vec![slot_keys[0].0; occupants.len()]
        } else {
            match crate::doc_id::pair_keys_by_fit(&slot_keys, &occupants, fits) {
                Some(pairing) => pairing.into_iter().map(|k| slot_keys[k].0).collect(),
                None => continue,
            }
        };
        for ((_, member), key) in entity
            .members
            .iter_mut()
            .enumerate()
            .filter(|(i, m)| holds(reshaped, *i, m, slot))
            .zip(handed)
        {
            let key = Some(key.clone());
            match (member, slot) {
                (Member::Method(m), DocSlot::Method { .. }) => m.xml_doc_sig = key,
                (Member::Property(p), DocSlot::Getter { .. } | DocSlot::ValField(_)) => {
                    p.xml_doc_sig = key
                }
                (Member::Property(p), DocSlot::Setter { .. }) => p.setter_xml_doc_sig = key,
                (Member::Event(e), DocSlot::Getter { .. }) => e.xml_doc_sig = key,
                _ => unreachable!("`occupies` pairs each slot with its member kind"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Units of measure in member types
// ---------------------------------------------------------------------------

/// What one scan over the pickled types of an entity's members found. A
/// measure, and an unreadable shape, are each final; the heads are what the
/// consumer must clear when neither was found.
#[derive(Debug, Default)]
struct MeasureScan {
    measured: bool,
    unknowable: bool,
    heads: std::collections::BTreeSet<NamedTypeHead>,
}

impl MeasureScan {
    fn reading(self) -> MemberMeasures {
        if self.measured {
            MemberMeasures::Measured
        } else if self.unknowable {
            MemberMeasures::Unknowable
        } else {
            MemberMeasures::NoDirectMeasure {
                heads: self.heads.into_iter().collect(),
            }
        }
    }

    /// Scan one pickled type. `collect_heads` is `false` for a member a
    /// consumer in another assembly cannot reach: its type is still searched
    /// for a measure, but a non-public type it names would not resolve for the
    /// consumer, and a public member cannot mention one (FS0410).
    fn scan(
        &mut self,
        pickled: &PickledCcu,
        local_fqns: &HashMap<u32, Vec<String>>,
        ty: &PickledType,
        collect_heads: bool,
    ) -> Result<(), ImportError> {
        match ty {
            PickledType::Measure(_) => self.measured = true,
            PickledType::Var { typar_index, .. } => {
                self.typar(pickled, *typar_index);
            }
            PickledType::Forall { typars, body } => {
                for &ti in typars {
                    self.typar(pickled, ti);
                }
                self.scan(pickled, local_fqns, body, collect_heads)?;
            }
            PickledType::AppSimple {
                simpletyp_index, ..
            } => {
                let nle = *pickled
                    .header
                    .simpletys
                    .get(*simpletyp_index as usize)
                    .ok_or(ImportError::OsgnIndexOutOfRange {
                        kind: "simplety (member measures)",
                        index: *simpletyp_index,
                        max: pickled.header.simpletys.len(),
                    })?;
                if collect_heads {
                    self.nonlocal_head(pickled, nle)?;
                }
            }
            PickledType::App { tcref, args, .. } => {
                if collect_heads {
                    match tcref {
                        PickledTcRef::NonLocal(nle) => self.nonlocal_head(pickled, *nle)?,
                        PickledTcRef::Local(stamp) => match local_fqns.get(stamp) {
                            Some(path) => {
                                self.heads.insert(NamedTypeHead {
                                    ccu: None,
                                    path: path.clone(),
                                });
                            }
                            // In range but not a recorded module or type: a
                            // shape no member type takes.
                            None if (*stamp as usize) < pickled.tables.tycons.len() => {
                                self.unknowable = true;
                            }
                            None => {
                                return Err(ImportError::OsgnIndexOutOfRange {
                                    kind: "local tycon (member measures)",
                                    index: *stamp,
                                    max: pickled.tables.tycons.len(),
                                });
                            }
                        },
                    }
                }
                for arg in args {
                    self.scan(pickled, local_fqns, arg, collect_heads)?;
                }
            }
            PickledType::Fun { domain, range, .. } => {
                self.scan(pickled, local_fqns, domain, collect_heads)?;
                self.scan(pickled, local_fqns, range, collect_heads)?;
            }
            PickledType::Tuple { elems, .. } => {
                for elem in elems {
                    self.scan(pickled, local_fqns, elem, collect_heads)?;
                }
            }
            // A union-case type occurs inside expressions, not in a member's
            // declared type.
            PickledType::UCase { .. } => self.unknowable = true,
        }
        Ok(())
    }

    /// A type parameter is a measure (or does not resolve, so might be one).
    fn typar(&mut self, pickled: &PickledCcu, index: u32) {
        match pickled.tables.typars.get(index as usize) {
            Some(t) if is_measure_typar(t.flags) => self.measured = true,
            Some(_) => {}
            None => self.unknowable = true,
        }
    }

    fn nonlocal_head(&mut self, pickled: &PickledCcu, nle: u32) -> Result<(), ImportError> {
        match decode_nonlocal_named(pickled, nle, Vec::new())? {
            AbbreviationTarget::Named { ccu, path, .. } => {
                self.heads.insert(NamedTypeHead { ccu, path });
            }
            _ => unreachable!("`decode_nonlocal_named` builds a `Named` target"),
        }
        Ok(())
    }

    fn field(
        &mut self,
        pickled: &PickledCcu,
        local_fqns: &HashMap<u32, Vec<String>>,
        field: &crate::fsharp_pickle::model::PickledRecdField,
    ) -> Result<(), ImportError> {
        self.scan(pickled, local_fqns, &field.ty, field.access.is_empty())
    }
}

/// Stamp each F# type's [`Entity::member_measures`] from the host signature
/// pickle: whether any type its fields and members are declared with mentions a
/// unit of measure, which the IL signatures the member projection reads have
/// erased.
///
/// What is scanned for a type is everything a member access on it can be typed
/// by: its own type parameters (a measure parameter is erased, so the type is
/// measure-parameterised under a measure-free IL name), its record, class,
/// union-case and exception fields, and every member val whose apparent parent
/// it is — properties, methods, constructors, abstract slots and augmentations
/// alike, all of which the type's enclosing module or namespace declares (an
/// object model's `vslots` are references back to those same vals). Scanning a
/// val that compiles elsewhere (an extension on this type, which lands on its
/// module) only declines more. What is scanned for a module is every val it
/// declares: its `let` values and functions, and any extension members, all of
/// which compile onto its class.
///
/// The row is the one [`PickleJoin`] certifies as the entity's own, and must be
/// a module exactly when the pickled entity is. A measure-parameterised type is
/// never stamped: its own typar is one IL erases, so `Unknowable` is the
/// reading that claims nothing about it. Anything not stamped keeps the
/// `Unknowable` the projector blanked it to.
pub(crate) fn apply_member_measures(
    entities: &mut [Entity],
    pickled: &PickledCcu,
    join: &PickleJoin,
) -> Result<(), ImportError> {
    struct Target {
        is_module: bool,
        scan: MeasureScan,
    }
    let local_fqns = local_tycon_fqns(pickled)?;
    let mut targets: HashMap<u32, Target> = HashMap::new();
    let mut member_vals: Vec<(PickledTcRef, u32)> = Vec::new();
    let mut parents: HashMap<u32, u32> = HashMap::new();
    let mut logical_paths: HashMap<u32, Vec<String>> = HashMap::new();
    let mut path = Vec::new();
    walk_entity_tree(
        pickled,
        pickled.root_entity,
        true,
        &[],
        &[],
        &mut path,
        &mut |stamp, entity, is_root, _namespace, _type_chain| {
            for &vi in &entity.module_type.vals {
                let v = pickled.tables.vals.get(vi as usize).ok_or(
                    ImportError::OsgnIndexOutOfRange {
                        kind: "val (member measures)",
                        index: vi,
                        max: pickled.tables.vals.len(),
                    },
                )?;
                if let Some(info) = &v.member_info {
                    member_vals.push((info.apparent_parent.clone(), vi));
                }
            }
            // The logical path a non-local reference back into this CCU names
            // the entity by; a parent is visited before its children.
            let path = match parents.get(&stamp) {
                Some(parent) => {
                    let mut p = logical_paths.get(parent).cloned().unwrap_or_default();
                    p.push(entity.logical_name.clone());
                    p
                }
                None => Vec::new(),
            };
            for &child in &entity.module_type.entities {
                parents.insert(child, stamp);
            }
            logical_paths.insert(stamp, path);
            let is_module_or_namespace = entity.flags & ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE != 0;
            // A module's members are its own `let` bindings, compiled onto its
            // class: every val it declares is scanned. That includes the
            // intrinsic members of any type nested in it, which compile onto
            // that type instead — scanning them here only declines more.
            if !is_root && is_module_or_namespace && entity.module_type.is_type != IsType::Namespace
            {
                let mut scan = MeasureScan::default();
                for &vi in &entity.module_type.vals {
                    let v = &pickled.tables.vals[vi as usize];
                    scan.scan(pickled, &local_fqns, &v.ty, v.access.is_empty())?;
                }
                targets.insert(
                    stamp,
                    Target {
                        is_module: true,
                        scan,
                    },
                );
                return Ok(());
            }
            if is_root
                || is_module_or_namespace
                || entity.type_abbrev.is_some()
                || !is_measure_free(pickled, entity)
            {
                return Ok(());
            }
            let mut scan = MeasureScan::default();
            let mut fields: Vec<&crate::fsharp_pickle::model::PickledRecdField> = Vec::new();
            match &entity.repr {
                PickledTyconRepr::Record(rfields) => fields.extend(rfields),
                PickledTyconRepr::Union(cases) => {
                    fields.extend(cases.iter().flat_map(|c| &c.fields));
                }
                PickledTyconRepr::FSharpObjectModel(model) => {
                    fields.extend(&model.rfields);
                }
                PickledTyconRepr::UnionWithStaticFields { cases, objmodel } => {
                    fields.extend(cases.iter().flat_map(|c| &c.fields));
                    fields.extend(&objmodel.rfields);
                }
                PickledTyconRepr::NoRepr => {}
                // An IL-backed or measure representation is no F# member
                // shape this reads.
                PickledTyconRepr::AsmRepr(_) | PickledTyconRepr::Measureable(_) => {
                    scan.unknowable = true;
                }
            }
            match &entity.exn_repr {
                PickledExnRepr::None => {}
                PickledExnRepr::Fresh(exn_fields) => fields.extend(exn_fields),
                PickledExnRepr::Abbrev(_) | PickledExnRepr::Asm(_) => scan.unknowable = true,
            }
            for field in fields {
                scan.field(pickled, &local_fqns, field)?;
            }
            targets.insert(
                stamp,
                Target {
                    is_module: false,
                    scan,
                },
            );
            Ok(())
        },
    )?;
    let host = pickled
        .tables
        .tycons
        .get(pickled.root_entity as usize)
        .map(|root| root.logical_name.as_str());
    let by_path: HashMap<&[String], u32> = logical_paths
        .iter()
        .map(|(stamp, path)| (path.as_slice(), *stamp))
        .collect();
    for (parent, vi) in member_vals {
        let stamp = match parent {
            PickledTcRef::Local(stamp) => Some(stamp),
            // An intrinsic member's apparent parent is pickled as a path into
            // its own CCU.
            PickledTcRef::NonLocal(index) => pickled
                .header
                .nlerefs
                .get(index as usize)
                .filter(|nle| {
                    pickled
                        .header
                        .ccu_refs
                        .get(nle.ccu as usize)
                        .is_some_and(|ccu| Some(ccu.name.as_str()) == host)
                })
                .and_then(|nle| {
                    nle.path
                        .iter()
                        .map(|&i| pickled.header.strings.get(i as usize).cloned())
                        .collect::<Option<Vec<String>>>()
                })
                .and_then(|path| by_path.get(path.as_slice()).copied()),
        };
        // A parent the walk did not record as a type is a module, a type this
        // overlay does not stamp, or an entity of another CCU.
        let Some(target) = stamp.and_then(|s| targets.get_mut(&s)) else {
            continue;
        };
        let v = &pickled.tables.vals[vi as usize];
        target
            .scan
            .scan(pickled, &local_fqns, &v.ty, v.access.is_empty())?;
    }
    let rows = join.rows(entities);
    for (stamp, target) in targets {
        let Some(ecma) = rows.entity_mut(entities, stamp) else {
            continue;
        };
        // The pickle and the row agree on whether this is a module, or the
        // reading describes members of some other shape than the row holds.
        if (ecma.kind == EntityKind::Module) != target.is_module {
            continue;
        }
        ecma.member_measures = target.scan.reading();
    }
    Ok(())
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

    /// The join a projection certifies when `entities` is the image's whole
    /// `TypeDef` table: every row the tree holds, nothing dropped.
    fn join_for(entities: &[Entity], ccu: &PickledCcu) -> Result<PickleJoin, ImportError> {
        fn rows(entities: &[Entity], parent: Option<&RowPath>, out: &mut Vec<RowPath>) {
            for e in entities {
                let path = match parent {
                    None => RowPath::of_top_level(e),
                    Some(p) => p.child(e),
                };
                if let Some(path) = &path {
                    out.push(path.clone());
                    rows(&e.nested_types, Some(path), out);
                }
            }
        }
        let mut table = Vec::new();
        rows(entities, None, &mut table);
        PickleJoin::certify(ccu, &table)
    }

    // The overlays as a projection runs them, joined against the tree they are
    // handed (`join_for`), so a test reads as the tree it builds.

    fn apply_entity_overlay(entities: &mut [Entity], ccu: &PickledCcu) -> Result<(), ImportError> {
        let join = join_for(entities, ccu)?;
        super::apply_entity_overlay(entities, ccu, &join)
    }

    fn apply_typar_constraints(
        entities: &mut Vec<Entity>,
        ccu: &PickledCcu,
    ) -> Result<(), ImportError> {
        let join = join_for(entities, ccu)?;
        super::apply_typar_constraints(entities, ccu, &join)
    }

    fn apply_union_cases(entities: &mut [Entity], ccu: &PickledCcu) -> Result<(), ImportError> {
        let join = join_for(entities, ccu)?;
        super::apply_union_cases(entities, ccu, &join)
    }

    fn apply_module_member_projection(
        entities: &mut [Entity],
        ccu: &PickledCcu,
        physical: &PhysicalMethods,
    ) -> Result<(), ImportError> {
        let join = join_for(entities, ccu)?;
        super::apply_module_member_projection(entities, ccu, &join, physical)
    }

    fn merge_measure_entities(
        entities: &mut [Entity],
        ccu: &PickledCcu,
    ) -> Result<(), ImportError> {
        let join = join_for(entities, ccu)?;
        super::merge_measure_entities(entities, ccu, &join)
    }

    fn apply_measure_overlay(entities: &mut [Entity], ccu: &PickledCcu) -> Result<(), ImportError> {
        let join = join_for(entities, ccu)?;
        super::apply_measure_overlay(entities, ccu, &join)
    }

    fn apply_declaration_order(
        entities: &mut [Entity],
        ccu: &PickledCcu,
    ) -> Result<(), ImportError> {
        let join = join_for(entities, ccu)?;
        super::apply_declaration_order(entities, ccu, &join)
    }

    fn apply_abbreviation_markers(
        entities: &mut Vec<Entity>,
        ccu: &PickledCcu,
        assembly: &AssemblyIdentity,
    ) -> Result<(), ImportError> {
        let join = join_for(entities, ccu)?;
        super::apply_abbreviation_markers(entities, ccu, &join, assembly)
    }

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

    /// A pickled entity. One with a namespace body, no representation and an
    /// ordinary kind is a namespace fragment, and carries the
    /// `IsModuleOrNamespace` flag a real one does; anything else built here
    /// starts without it (a measure, or an abbreviation once
    /// [`make_abbreviation`] gives it a target, is a *type* with a namespace
    /// body).
    fn make_entity_kinded(
        logical_name: &str,
        repr: PickledTyconRepr,
        modul_typ: PickledModulType,
        typar_kind: TyparKind,
    ) -> PickledEntity {
        let is_namespace_fragment = modul_typ.is_type == IsType::Namespace
            && matches!(repr, PickledTyconRepr::NoRepr)
            && typar_kind == TyparKind::Type;
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
            flags: if is_namespace_fragment {
                ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE
            } else {
                0
            },
            cpath: None,
            module_type: modul_typ,
            exn_repr: PickledExnRepr::None,
            xmldoc: None,
        }
    }

    fn make_ecma_entity(namespace: Vec<&str>, name: &str, kind: EntityKind) -> Entity {
        Entity {
            extension_member_names: Vec::new(),
            union_cases: UnionCases::Unknowable,
            union_case_makers: Vec::new(),
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
            metadata_name: Some(name.to_string()),
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            is_no_pia_local: false,
            obsolete: None,
            compiler_message: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: Vec::new(),
            source_name: None,
            custom_attrs: Vec::new(),
            abbreviation_target: None,
            definition_range: None,
            member_measures: crate::model::MemberMeasures::NotFSharp,
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

    /// The [`CurriedArgs`] of groups of the given sizes, every argument unnamed
    /// (as [`val_repr_info`] pickles them).
    fn unnamed_groups(leading_receiver: bool, sizes: &[usize]) -> CurriedArgs {
        CurriedArgs {
            leading_receiver,
            groups: sizes
                .iter()
                .map(|&n| ArgGroup {
                    args: (0..n).map(|_| CurriedArg { name: None }).collect(),
                })
                .collect(),
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
        Member::Method(Box::new(MethodLike {
            val_facts: crate::model::ValFacts::NotFSharp,
            definition_range: None,
            name: name.to_string(),
            access: Access::Public,
            signature: MethodSignature {
                parameters: Vec::new(),
                return_type: TypeRef::Primitive(Primitive::Void),
                return_nullability: Nullability::Oblivious,
            },
            arg_group_count: Some(1),
            arg_groups: crate::model::ArgGroups::NotFSharp,
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
            xml_doc_sig: None,
            drops_optional_modifier: false,
            custom_operation: None,
            has_other_method_impl: false,
        }))
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
        apply_module_member_projection(&mut entities, &ccu, &PhysicalMethods::new())
            .expect("member projection");
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

    #[test]
    fn a_logical_name_carries_its_arity_after_the_last_backtick() {
        assert_eq!(logical_name_arity("List`1"), Some(1));
        assert_eq!(logical_name_arity("Dictionary`2"), Some(2));
        assert_eq!(logical_name_arity("KeyCollection"), Some(0));
        assert_eq!(logical_name_arity("Odd`name`3"), Some(3));
        // No digits after the backtick is no arity suffix.
        assert_eq!(logical_name_arity("Odd`"), Some(0));
        assert_eq!(logical_name_arity("Odd`x"), Some(0));
        assert_eq!(logical_name_arity("Huge`99999999999999999999999"), None);
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
                    curried: Some(unnamed_groups(false, &[1])),
                    is_instance: false,
                    is_member: false,
                    is_extension: false,
                    target_typars: None,
                    is_generic: false,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                    xml_doc_sig: None,
                    measure_typars: 0,
                    typar_constraints: Some(vec![]),
                    val_facts: crate::model::ValFacts::Read(Box::new(ReadValFacts {
                        measures: ValMeasures::NoDirectMeasure { heads: vec![] },
                        compiler_message: None,
                        requires_explicit_type_arguments: false,
                    })),
                },
                ModuleMemberVal {
                    val_index: 1,
                    logical_name: "Doubled".to_string(),
                    compiled_name: Some("Counter.Doubled".to_string()),
                    // Receiver group + erased-unit group.
                    il_arity: Some(1),
                    arg_group_count: Some(2),
                    curried: Some(unnamed_groups(true, &[0])),
                    is_instance: true,
                    is_member: true,
                    is_extension: true,
                    target_typars: Some(0),
                    is_generic: false,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                    xml_doc_sig: None,
                    measure_typars: 0,
                    typar_constraints: Some(vec![]),
                    val_facts: crate::model::ValFacts::Read(Box::new(ReadValFacts {
                        measures: ValMeasures::NoDirectMeasure { heads: vec![] },
                        compiler_message: None,
                        requires_explicit_type_arguments: false,
                    })),
                },
                ModuleMemberVal {
                    val_index: 2,
                    logical_name: "Make".to_string(),
                    compiled_name: Some("Counter.Make.Static".to_string()),
                    il_arity: None,
                    arg_group_count: None,
                    curried: None,
                    is_instance: false,
                    is_member: true,
                    is_extension: true,
                    target_typars: Some(0),
                    is_generic: false,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                    xml_doc_sig: None,
                    measure_typars: 0,
                    typar_constraints: Some(vec![]),
                    val_facts: crate::model::ValFacts::Read(Box::new(ReadValFacts {
                        measures: ValMeasures::NoDirectMeasure { heads: vec![] },
                        compiler_message: None,
                        requires_explicit_type_arguments: false,
                    })),
                },
                ModuleMemberVal {
                    val_index: 3,
                    logical_name: "id".to_string(),
                    compiled_name: None,
                    il_arity: Some(1),
                    arg_group_count: Some(1),
                    curried: Some(unnamed_groups(false, &[1])),
                    is_instance: false,
                    is_member: false,
                    is_extension: false,
                    target_typars: None,
                    is_generic: true,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                    xml_doc_sig: None,
                    measure_typars: 0,
                    typar_constraints: Some(vec![]),
                    val_facts: crate::model::ValFacts::Read(Box::new(ReadValFacts {
                        measures: ValMeasures::NoDirectMeasure { heads: vec![] },
                        compiler_message: None,
                        requires_explicit_type_arguments: false,
                    })),
                },
                ModuleMemberVal {
                    val_index: 4,
                    logical_name: "helper".to_string(),
                    compiled_name: None,
                    il_arity: None,
                    arg_group_count: None,
                    curried: None,
                    is_instance: false,
                    is_member: false,
                    is_extension: false,
                    target_typars: None,
                    is_generic: false,
                    is_literal: false,
                    is_public: true,
                    definition_range: None,
                    xml_doc_sig: None,
                    measure_typars: 0,
                    typar_constraints: Some(vec![]),
                    val_facts: crate::model::ValFacts::Read(Box::new(ReadValFacts {
                        measures: ValMeasures::NoDirectMeasure { heads: vec![] },
                        compiler_message: None,
                        requires_explicit_type_arguments: false,
                    })),
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

        apply_module_member_projection(&mut entities, &ccu, &PhysicalMethods::new())
            .expect("member projection");

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
        apply_module_member_projection(&mut entities, &ccu, &PhysicalMethods::new())
            .expect("member projection");
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
        apply_module_member_projection(&mut entities, &ccu, &PhysicalMethods::new())
            .expect("member projection");
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

    /// Doc-comment keys ride the claim like definition ranges: a unanimous
    /// claim group stamps its val's pickled `XmlDocSig`, an undocumented val
    /// (empty sig) stamps nothing, and a same-`(name, shape)` group whose
    /// documented overloads carry different keys hands each method the key its
    /// signature fits — whichever val claimed it, since claiming is by name and
    /// arity — or, where the signatures do not force the pairing (both
    /// `Shared` methods take an `int`), under-sets: handing either method the
    /// other's key would show the sibling's docs. A val whose own method the
    /// projection elided, beside a hidden method of the same slot, stamps
    /// nothing either: the physical census counts two methods for one val,
    /// though only the hidden one is projected.
    #[test]
    fn member_list_stamps_doc_sigs_by_claim_group() {
        let mut value = ext_test_val_arity(None, 0, None, &[]);
        value.logical_name = "answer".to_string();
        value.xmldoc_sig = "P:NS.M.answer".to_string();
        let mut bare = ext_test_val_arity(Some("bare"), 0, None, &[1]);
        bare.logical_name = "bare".to_string();
        let mut a = ext_test_val_arity(Some("Shared"), 0, None, &[1]);
        a.logical_name = "first".to_string();
        a.xmldoc_sig = "M:NS.M.Shared(System.Int32)".to_string();
        let mut b = ext_test_val_arity(Some("Shared"), 0, None, &[1]);
        b.logical_name = "second".to_string();
        b.xmldoc_sig = "M:NS.M.Shared(System.String)".to_string();
        let mut lone = ext_test_val_arity(Some("Lone"), 0, None, &[1]);
        lone.logical_name = "lone".to_string();
        lone.xmldoc_sig = "M:NS.M.Lone(System.Int32)".to_string();
        // Declared in the opposite order to their methods, so the first val
        // claims the method whose signature fits the second's key.
        let mut c = ext_test_val_arity(Some("Paired"), 0, None, &[1]);
        c.logical_name = "third".to_string();
        c.xmldoc_sig = "M:NS.M.Paired(System.String)".to_string();
        let mut d = ext_test_val_arity(Some("Paired"), 0, None, &[1]);
        d.logical_name = "fourth".to_string();
        d.xmldoc_sig = "M:NS.M.Paired(System.Int32)".to_string();

        let module = make_entity(
            "M",
            PickledTyconRepr::NoRepr,
            module_with_vals(vec![0, 1, 2, 3, 4, 5, 6]),
        );
        let mut ns_modul = empty_modul_typ();
        ns_modul.entities = vec![2];
        let ns = make_entity("NS", PickledTyconRepr::NoRepr, ns_modul);
        let mut root_modul = empty_modul_typ();
        root_modul.entities = vec![1];
        let root = make_entity("Test", PickledTyconRepr::NoRepr, root_modul);
        let ccu = make_ccu(
            vec![root, ns, module],
            vec![value, bare, a, b, lone, c, d],
            0,
        );

        // The physical methods: one per projected member, plus `lone`'s own
        // `Lone`, which the projection elided — the projected `Lone` is a
        // hidden helper.
        let physical_slots = [
            ("get_answer", 0),
            ("bare", 1),
            ("Shared", 1),
            ("Shared", 1),
            ("Lone", 1),
            ("Paired", 1),
            ("Paired", 1),
            ("Lone", 1),
        ];
        let string_param = |mut m: Member| {
            if let Member::Method(mm) = &mut m {
                mm.signature.parameters[0].ty = TypeRef::Primitive(Primitive::String);
            }
            m
        };
        let physical: PhysicalMethods = physical_slots
            .iter()
            .enumerate()
            .map(|(i, (name, params))| {
                (
                    i as u32,
                    PhysicalMethod {
                        name: name.to_string(),
                        params: Some(*params),
                        is_static: true,
                        accessor_of: None,
                    },
                )
            })
            .collect();
        let mut entities = vec![{
            let mut e = make_ecma_entity(vec!["NS"], "M", EntityKind::Module);
            e.members = vec![
                make_ecma_module_value("answer"),
                make_ecma_method_arity("bare", 1),
                make_ecma_method_arity("Shared", 1),
                make_ecma_method_arity("Shared", 1),
                make_ecma_method_arity("Lone", 1),
                make_ecma_method_arity("Paired", 1),
                string_param(make_ecma_method_arity("Paired", 1)),
            ];
            e.method_def_tokens = (0..physical_slots.len() as u32).collect();
            e
        }];
        apply_module_member_projection(&mut entities, &ccu, &physical).expect("member projection");
        let entity = entities.into_iter().next().expect("one entity");
        let sigs: Vec<(&str, Option<&str>)> = entity
            .members
            .iter()
            .filter_map(|m| match m {
                Member::Method(m) => Some((m.name.as_str(), m.xml_doc_sig.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            sigs,
            vec![
                ("answer", Some("P:NS.M.answer")),
                ("bare", None),
                ("Shared", None),
                ("Shared", None),
                ("Lone", None),
                ("Paired", Some("M:NS.M.Paired(System.Int32)")),
                ("Paired", Some("M:NS.M.Paired(System.String)")),
            ]
        );
    }

    /// What became of a pickled val's own IL member in the projection.
    #[derive(Debug, Clone, Copy)]
    enum ValMember {
        Kept,
        /// Refused, and recorded in `skipped_members`.
        Refused,
        /// Dropped without a record (a deliberate elision).
        Elided,
    }

    /// One generated IL method: a name, a parameter list over two types, and
    /// staticness — the method's IL identity — plus either the pickled val it
    /// compiles from (documented or not) or nothing, for a member the signature
    /// file hides.
    #[derive(Debug, Clone)]
    struct DocSlotItem {
        name: u8,
        /// Parameter types: `System.Int32`, `System.String`, and 2-D and 3-D
        /// `int` arrays — whose keys, in current fsc's spelling, the parameter
        /// check alone cannot always tell apart.
        params: Vec<u8>,
        is_static: bool,
        val: Option<(bool, ValMember)>,
    }

    fn doc_slot_item() -> impl Strategy<Value = DocSlotItem> {
        let member = prop_oneof![
            Just(ValMember::Kept),
            Just(ValMember::Refused),
            Just(ValMember::Elided),
        ];
        (
            0u8..2,
            proptest::collection::vec(0u8..4, 0..3),
            any::<bool>(),
            proptest::option::of((any::<bool>(), member)),
        )
            .prop_map(|(name, params, is_static, val)| DocSlotItem {
                name,
                params,
                is_static,
                val,
            })
    }

    proptest! {
        // The hazards need two items in one slot with particular fates, so the
        // default case count reaches them only sometimes.
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// Soundness of the type-member stamp against a generated ground truth.
        /// Whatever mix of overloads, undocumented vals, refused or silently
        /// elided members and signature-hidden IL members a type holds, a member
        /// that receives a key is the member of a val that carries that key.
        /// Each val's key encodes its parameter types as fsc writes them, and no
        /// two IL methods share an identity, as in real metadata.
        ///
        /// And liveness, where the answer is certain: a slot whose every method
        /// is a documented val's kept member, with no array parameter whose key
        /// spelling could fit two ranks and no refused member of its name,
        /// stamps each method with its own key — overloads included, since
        /// their keys spell different parameters.
        #[test]
        fn type_member_stamp_never_hands_a_member_another_vals_key(
            items in proptest::collection::vec(doc_slot_item(), 0..8)
        ) {
            let mut seen = std::collections::HashSet::new();
            let items: Vec<DocSlotItem> = items
                .into_iter()
                .filter(|i| seen.insert((i.name, i.params.clone(), i.is_static)))
                .collect();
            let ty = |p: u8| match p {
                0 => "System.Int32",
                1 => "System.String",
                2 => "System.Int32[0:]",
                _ => "System.Int32[0:, 0:]",
            };
            let key_of = |i: &DocSlotItem| {
                let params: Vec<&str> = i.params.iter().map(|p| ty(*p)).collect();
                if params.is_empty() {
                    format!("M:N.T.M{}", i.name)
                } else {
                    format!("M:N.T.M{}({})", i.name, params.join(","))
                }
            };
            let method_of = |i: &DocSlotItem| {
                let mut m = make_ecma_method(&format!("M{}", i.name));
                if let Member::Method(mm) = &mut m {
                    mm.is_static = i.is_static;
                    mm.signature.parameters = i
                        .params
                        .iter()
                        .map(|p| Parameter {
                            name: None,
                            ty: match p {
                                0 => TypeRef::Primitive(Primitive::I4),
                                1 => TypeRef::Primitive(Primitive::String),
                                rank => TypeRef::Array {
                                    element: Box::new(crate::model::NullableType::oblivious(TypeRef::Primitive(
                                        Primitive::I4,
                                    ))),
                                    rank: *rank,
                                    sizes: Vec::new(),
                                    lower_bounds: Vec::new(),
                                },
                            },
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
            };
            let mut entity = make_ecma_entity(vec!["N"], "T", EntityKind::Class);
            // Every item is a physical MethodDef, whatever the projection made
            // of it.
            let physical: PhysicalMethods = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    (
                        i as u32,
                        PhysicalMethod {
                            name: format!("M{}", item.name),
                            params: Some(item.params.len()),
                            is_static: item.is_static,
                            accessor_of: None,
                        },
                    )
                })
                .collect();
            entity.method_def_tokens = (0..items.len() as u32).collect();
            let mut keys = Vec::new();
            // The key each projected member truly carries: `Some(k)` for the
            // kept member of a val keyed `k`, `None` otherwise.
            let mut truth = Vec::new();
            // Per projected member, the item it compiles.
            let mut owners = Vec::new();
            for item in &items {
                let slot = DocSlot::Method {
                    name: format!("M{}", item.name),
                    arity: item.params.len(),
                    is_static: item.is_static,
                    compiled: Compiled::AsDeclared,
                };
                match item.val {
                    Some((documented, member)) => {
                        let key = documented.then(|| key_of(item));
                        keys.push(DocKey {
                            slot,
                            key: key.clone(),
                            erased_typars: 0,
                            curried: None,
                            role: SlotRole::Plain,
                        });
                        match member {
                            ValMember::Kept => {
                                entity.members.push(method_of(item));
                                truth.push(key);
                                owners.push(item);
                            }
                            ValMember::Refused => entity.skipped_members.push(SkippedMember {
                                name: format!("M{}", item.name),
                                reason: "refused".to_string(),
                            }),
                            ValMember::Elided => {}
                        }
                    }
                    None => {
                        entity.members.push(method_of(item));
                        truth.push(None);
                        owners.push(item);
                    }
                }
            }
            let reshaped = vec![false; entity.members.len()];
            stamp_doc_sigs(&mut entity, &keys, &physical, &reshaped);
            let same_slot = |a: &DocSlotItem, b: &DocSlotItem| {
                (a.name, a.params.len(), a.is_static) == (b.name, b.params.len(), b.is_static)
            };
            // A refused member declines every slot of its name, whatever its
            // arity or staticness: the record names it, nothing more.
            let certain = |owner: &DocSlotItem| {
                items.iter().filter(|i| same_slot(i, owner)).all(|i| {
                    matches!(i.val, Some((true, ValMember::Kept))) && i.params.iter().all(|p| *p < 2)
                }) && !items
                    .iter()
                    .any(|i| i.name == owner.name && matches!(i.val, Some((_, ValMember::Refused))))
            };
            for ((member, truth), owner) in entity.members.iter().zip(&truth).zip(&owners) {
                let Member::Method(m) = member else {
                    unreachable!("the generator builds methods only")
                };
                if let Some(stamped) = &m.xml_doc_sig {
                    prop_assert_eq!(Some(stamped), truth.as_ref(), "{:?}", items);
                }
                if certain(owner) {
                    prop_assert_eq!(m.xml_doc_sig.as_ref(), truth.as_ref(), "{:?}", items);
                }
            }
        }

        /// Liveness of the overload pairing, which the soundness property's
        /// generator rarely reaches: two to four documented overloads sharing a
        /// slot, each a kept member with its own `int`/`string` parameter list,
        /// in any order relative to their vals, are each stamped with their own
        /// val's key.
        #[test]
        fn type_member_stamp_pairs_documented_overloads(
            (arity, params, order) in (1usize..4).prop_flat_map(|arity| {
                let lists = proptest::collection::vec(
                    proptest::collection::vec(any::<bool>(), arity),
                    2..5,
                );
                (Just(arity), lists).prop_flat_map(|(arity, lists)| {
                    let mut seen = std::collections::HashSet::new();
                    let lists: Vec<Vec<bool>> =
                        lists.into_iter().filter(|l| seen.insert(l.clone())).collect();
                    let order = Just((0..lists.len()).collect::<Vec<usize>>()).prop_shuffle();
                    (Just(arity), Just(lists), order)
                })
            })
        ) {
            prop_assume!(params.len() >= 2);
            let enc = |is_string: bool| if is_string { "System.String" } else { "System.Int32" };
            let key_of = |list: &[bool]| {
                let encoded: Vec<&str> = list.iter().map(|s| enc(*s)).collect();
                format!("M:N.T.M({})", encoded.join(","))
            };
            let mut entity = make_ecma_entity(vec!["N"], "T", EntityKind::Class);
            for list in &params {
                let mut m = make_ecma_method_arity("M", arity);
                if let Member::Method(mm) = &mut m {
                    for (p, is_string) in mm.signature.parameters.iter_mut().zip(list) {
                        if *is_string {
                            p.ty = TypeRef::Primitive(Primitive::String);
                        }
                    }
                }
                entity.members.push(m);
            }
            let physical: PhysicalMethods = (0..params.len() as u32)
                .map(|t| {
                    (
                        t,
                        PhysicalMethod {
                            name: "M".to_string(),
                            params: Some(arity),
                            is_static: true,
                            accessor_of: None,
                        },
                    )
                })
                .collect();
            entity.method_def_tokens = (0..params.len() as u32).collect();
            // The vals in `order`: the pickle's order need not be the IL's.
            let keys: Vec<DocKey> = order
                .iter()
                .map(|&i| DocKey {
                    slot: DocSlot::Method {
                        name: "M".to_string(),
                        arity,
                        is_static: true,
                        compiled: Compiled::AsDeclared,
                    },
                    key: Some(key_of(&params[i])),
                    erased_typars: 0,
                    curried: None,
                    role: SlotRole::Plain,
                })
                .collect();
            let reshaped = vec![false; entity.members.len()];
            stamp_doc_sigs(&mut entity, &keys, &physical, &reshaped);
            for (member, list) in entity.members.iter().zip(&params) {
                let Member::Method(m) = member else {
                    unreachable!("the test builds methods only")
                };
                prop_assert_eq!(m.xml_doc_sig.clone(), Some(key_of(list)), "{:?}", params);
            }
        }
    }

    /// One generated type method for the argument-group stamp: its IL identity
    /// (a name, staticness, and a parameter count fixed by the grouping), and
    /// either the val it compiles from — its groups, each argument named or
    /// not, and what the projection made of its member — or nothing, for a
    /// member the signature file hides.
    #[derive(Debug, Clone)]
    struct GroupSlotItem {
        name: u8,
        is_static: bool,
        /// Each group's argument names (`None`: unnamed). The parameter count
        /// is their total.
        groups: Vec<Vec<Option<u8>>>,
        val: Option<ValMember>,
        /// Whether the val is an `abstract` slot with its `default`: two vals,
        /// one IL method.
        paired: bool,
        /// Whether the val is an instance member fsc compiled as a static
        /// taking the receiver first: its IL method is static and one
        /// parameter wider. Only for a plain (unpaired) val; a hidden method
        /// keeps the staticness drawn.
        receiver_first: bool,
    }

    fn group_slot_item() -> impl Strategy<Value = GroupSlotItem> {
        let member = prop_oneof![
            4 => Just(ValMember::Kept),
            1 => Just(ValMember::Refused),
            1 => Just(ValMember::Elided),
        ];
        (
            0u8..2,
            any::<bool>(),
            proptest::collection::vec(
                proptest::collection::vec(proptest::option::of(0u8..3), 0..3),
                0..3,
            ),
            proptest::option::of(member),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(|(name, is_static, groups, val, paired, receiver_first)| {
                let receiver_first = receiver_first && val.is_some() && !paired;
                GroupSlotItem {
                    name,
                    // A receiver-first val's IL method is static.
                    is_static: is_static || receiver_first,
                    groups,
                    val,
                    paired,
                    receiver_first,
                }
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// Soundness of the argument-group stamp against a generated ground
        /// truth: whatever mix of overloads (curried or not, named alike or
        /// not), refused or silently elided members and signature-hidden IL
        /// members a type holds, a method that receives a grouping is a val's,
        /// grouped as that val groups its arguments, and named as it names them
        /// — or, where several vals share its slot, as its own IL names its
        /// parameters. Receiver-first vals run through the whole row overlay,
        /// so a member reshaped from one stays that val's, and a val whose own
        /// method was elided claims nobody else's.
        #[test]
        fn type_member_arg_group_stamp_hands_each_method_its_own_vals_grouping(
            items in proptest::collection::vec(group_slot_item(), 0..8)
        ) {
            // Items may share a slot: those are overloads, as many as drawn.
            let arity = |i: &GroupSlotItem| i.groups.iter().map(Vec::len).sum::<usize>();
            let curried_of = |i: &GroupSlotItem| CurriedArgs {
                leading_receiver: false,
                groups: i
                    .groups
                    .iter()
                    .map(|g| ArgGroup {
                        args: g
                            .iter()
                            .map(|n| CurriedArg { name: n.map(|n| format!("p{n}")) })
                            .collect(),
                    })
                    .collect(),
            };
            let receiver = TypeRef::Named {
                assembly: None,
                namespace: vec!["N".to_string()],
                name: "T".to_string(),
                type_args: Vec::new(),
                segment_arities: vec![0],
            };
            // The method's IL parameter count, receiver included.
            let il_arity = |i: &GroupSlotItem| arity(i) + usize::from(i.receiver_first);
            let method_of = |k: usize, i: &GroupSlotItem| {
                let mut m = make_ecma_method(&format!("M{}", i.name));
                if let Member::Method(mm) = &mut m {
                    mm.is_static = i.is_static;
                    mm.arg_groups = ArgGroups::Unknowable;
                    // Tags the member with its item, for the oracle below.
                    mm.metadata_token = k as u32;
                    let param = |name: String, ty: TypeRef| Parameter {
                        name: Some(name),
                        ty,
                        is_byref: false,
                        is_out: false,
                        is_readonly_ref: false,
                        default: crate::model::ParamDefault::None,
                        is_param_array: false,
                        nullability: Nullability::Oblivious,
                    };
                    mm.signature.parameters = i
                        .receiver_first
                        .then(|| param("this".to_string(), receiver.clone()))
                        .into_iter()
                        .chain(
                            (0..arity(i))
                                .map(|p| param(format!("il{k}_{p}"), TypeRef::Primitive(Primitive::I4))),
                        )
                        .collect();
                }
                m
            };
            let mut entity = make_ecma_entity(vec!["N"], "T", EntityKind::Class);
            let physical: PhysicalMethods = items
                .iter()
                .enumerate()
                .map(|(k, item)| {
                    (
                        k as u32,
                        PhysicalMethod {
                            name: format!("M{}", item.name),
                            params: Some(il_arity(item)),
                            is_static: item.is_static,
                            accessor_of: None,
                        },
                    )
                })
                .collect();
            entity.method_def_tokens = (0..items.len() as u32).collect();
            let slot_of = |i: &GroupSlotItem| DocSlot::Method {
                name: format!("M{}", i.name),
                arity: arity(i),
                is_static: i.is_static && !i.receiver_first,
                compiled: if i.receiver_first {
                    Compiled::StaticWithReceiver
                } else {
                    Compiled::AsDeclared
                },
            };
            let mut keys = Vec::new();
            // Per item: its own val's grouping, if it has a val.
            let mut truth = Vec::new();
            for (k, item) in items.iter().enumerate() {
                match item.val {
                    Some(member) => {
                        let roles: &[SlotRole] = if item.paired {
                            &[SlotRole::DispatchSlot, SlotRole::Override]
                        } else {
                            &[SlotRole::Plain]
                        };
                        for &role in roles {
                            keys.push(DocKey {
                                slot: slot_of(item),
                                key: None,
                                erased_typars: 0,
                                curried: Some(curried_of(item)),
                                role,
                            });
                        }
                        truth.push(Some(curried_of(item)));
                        match member {
                            ValMember::Kept => entity.members.push(method_of(k, item)),
                            ValMember::Refused => entity.skipped_members.push(SkippedMember {
                                name: format!("M{}", item.name),
                                reason: "refused".to_string(),
                            }),
                            ValMember::Elided => {}
                        }
                    }
                    None => {
                        entity.members.push(method_of(k, item));
                        truth.push(None);
                    }
                }
            }
            let shared: std::collections::HashMap<DocSlot, usize> =
                keys.iter().fold(std::collections::HashMap::new(), |mut acc, k| {
                    *acc.entry(k.slot.clone()).or_default() += 1;
                    acc
                });
            let target = TypeMemberTarget {
                namespace: vec!["N".to_string()],
                type_chain: vec!["T".to_string()],
                arity: 0,
                stamp: 0,
                keys,
            };
            overlay_type_row(&mut entity, &target, &physical);
            for member in &entity.members {
                let Member::Method(m) = member else {
                    unreachable!("the generator builds methods only")
                };
                let k = m.metadata_token as usize;
                let (item, truth) = (&items[k], &truth[k]);
                let ArgGroups::Read(stamped) = &m.arg_groups else {
                    continue;
                };
                let Some(truth) = truth else {
                    prop_assert!(false, "a hidden member got {:?}: {:?}", stamped, items);
                    unreachable!();
                };
                let sizes = |c: &CurriedArgs| {
                    c.groups.iter().map(|g| g.args.len()).collect::<Vec<_>>()
                };
                prop_assert_eq!(sizes(stamped), sizes(truth), "{:?}", items);
                prop_assert!(stamped.split(&m.signature.parameters).is_some());
                if **stamped != *truth {
                    prop_assert!(
                        shared.get(&slot_of(item)).copied().unwrap_or(0) > 1,
                        "a lone val's method got another's names: {:?} for {:?}",
                        stamped,
                        truth
                    );
                    let il: Vec<Option<String>> =
                        m.signature.parameters.iter().map(|p| p.name.clone()).collect();
                    let names: Vec<Option<String>> = stamped
                        .groups
                        .iter()
                        .flat_map(|g| g.args.iter().map(|a| a.name.clone()))
                        .collect();
                    prop_assert_eq!(names, il);
                }
            }
        }
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
        apply_module_member_projection(&mut entities, &ccu, &PhysicalMethods::new())
            .expect("member projection");
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
        // The projector stores `Entity::name` arity-stripped, and the row's own
        // name in `metadata_name`.
        let mut owned = vec![make_ecma_entity(
            vec!["N"],
            "FSharpChoice",
            EntityKind::Class,
        )];
        owned[0].metadata_name = Some("FSharpChoice`2".to_string());
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

        let err =
            apply_module_member_projection(&mut [], &ccu, &PhysicalMethods::new()).unwrap_err();
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
        e.flags = 0;
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
    fn fsharp_core_synthesises_markers_like_any_other_assembly() {
        // FSharp.Core's abbreviations (`type int = int32`, `type 'T option =
        // Option<'T>`, …) are ordinary pickled abbreviations: without markers
        // they resolve to nothing at all (they have no ECMA TypeDef), and the
        // sema chase through `abbreviation_target` is what gives `int` and
        // `option` their semantics — not a hard-coded alias table.
        let pickled = ccu_with_abbreviations();
        let mut core = dummy_assembly();
        core.name = "FSharp.Core".to_string();
        let mut as_core = abbreviation_ecma_tree();
        apply_abbreviation_markers(&mut as_core, &pickled, &core).expect("marker synthesis");
        let mut as_other = abbreviation_ecma_tree();
        apply_abbreviation_markers(&mut as_other, &pickled, &dummy_assembly())
            .expect("marker synthesis");
        // The trees differ only by the host identity every entity carries;
        // normalising it makes the comparison exact rather than shape-ish.
        fn stamp(entities: &mut [Entity], identity: &AssemblyIdentity) {
            for e in entities {
                e.assembly = identity.clone();
                stamp(&mut e.nested_types, identity);
            }
        }
        stamp(&mut as_core, &dummy_assembly());
        assert_eq!(
            as_core, as_other,
            "FSharp.Core gets exactly the markers any other assembly would"
        );
        assert_ne!(
            as_other,
            abbreviation_ecma_tree(),
            "the pickle's abbreviations must actually synthesise markers"
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
    /// An opaque *type* — a signature-hidden representation — pickled with a
    /// namespace body: `NoRepr`, and `IsModuleOrNamespace` clear.
    fn opaque_type(name: &str) -> PickledEntity {
        let mut e = make_entity(name, PickledTyconRepr::NoRepr, empty_modul_typ());
        e.flags = 0;
        e
    }

    fn ccu_with_hidden_union() -> PickledCcu {
        let teq = opaque_type("Teq");
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
        // `.fs`, pickles the SIGNATURE as `NoRepr`, so `apply_union_cases`
        // never reaches it via the union-repr walk and it kept
        // `UnionCases::Unknowable` — read downstream (`assembly_env.rs`
        // `fold_tycon_tier`) as "unknowable hidden cases", which made `open`ing
        // the namespace defer every dotted head (e.g. `List.replicate` where a
        // file-local union case `List` forces the dotted-path branch). But a
        // representation hidden by the signature exposes ZERO accessible cases,
        // so the answer is a knowably-empty `UnionCases::Known(vec![])`, exactly like a
        // private representation's private-case filter. A same-shaped `NoRepr`
        // module (the `IsModuleOrNamespace` flag set) must stay untouched — it is
        // never even a candidate.
        let pickled = ccu_with_hidden_union();
        let mut entities = vec![
            make_ecma_entity(vec!["Ns"], "Teq", EntityKind::Union),
            make_ecma_entity(vec!["Ns"], "Helpers", EntityKind::Module),
        ];
        apply_union_cases(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_cases,
            UnionCases::Known(Vec::new()),
            "a signature-hidden union has knowably zero accessible cases"
        );
        assert_eq!(
            entities[1].union_cases,
            UnionCases::Unknowable,
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
        apply_union_cases(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_cases,
            UnionCases::Unknowable,
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
        let mut u = opaque_type("U");
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
        apply_union_cases(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_cases,
            UnionCases::Unknowable,
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

    /// End to end: the renamed measure type's ECMA row takes its source name.
    #[test]
    fn the_source_name_overlay_reaches_a_renamed_measure_type() {
        let mut renamed = typed_entity_with_typars("SourceQ", vec![0]);
        renamed.compiled_name = Some("ClrQ".to_string());
        let [root, ns] = root_and_namespace(vec![2]);
        let mut ccu = make_ccu(vec![root, ns, renamed], Vec::new(), 0);
        ccu.tables.typars = vec![make_typar(MEASURE_TYPAR_FLAGS)];

        // The metadata row: arity 0, because the measure typar is erased.
        let mut owned = vec![make_ecma_entity(vec!["N"], "ClrQ", EntityKind::Class)];
        apply_entity_overlay(&mut owned, &ccu).expect("overlay");
        assert_eq!(
            owned[0].source_name.as_deref(),
            Some("SourceQ"),
            "the row keeps the name F# source writes, not the compiled one"
        );
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
        let mut u = opaque_type("U");
        u.typars = vec![0]; // one MEASURE typar
        let v = opaque_type("V"); // measure-free
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
        apply_union_cases(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_cases,
            UnionCases::Unknowable,
            "a measure-parameterised hidden union is declined (kept unknowable)"
        );
        assert_eq!(
            entities[1].union_cases,
            UnionCases::Known(Vec::new()),
            "a measure-free hidden union is still sealed"
        );
    }

    #[test]
    fn the_seal_never_reaches_a_union_whose_name_strips_alike() {
        // codex round 6: the projected key `(strip_arity(name),
        // generic_parameters.len())` is not injective. A legal `type U` (opaque
        // sealed class) beside `[<CompiledName("U`0")>] type Other = A | B` yields
        // distinct metadata rows `U` and `U`0` that BOTH project to
        // `(name = "U", arity = 0)`. The opaque type's row is the one named
        // exactly `U` — a class, so nothing is sealed — and the union at `U`0`
        // keeps its unknowable cases (the row a seal keyed by name would have
        // overwritten).
        let opaque = opaque_type("U");
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
        let mut renamed_union = make_ecma_entity(vec!["Ns"], "U", EntityKind::Union);
        renamed_union.metadata_name = Some("U`0".to_string());
        let mut entities = vec![
            make_ecma_entity(vec!["Ns"], "U", EntityKind::Class),
            renamed_union,
        ];
        apply_union_cases(&mut entities, &pickled).expect("apply union case names");
        assert_eq!(
            entities[0].union_cases,
            UnionCases::Unknowable,
            "a class is not sealed"
        );
        assert_eq!(
            entities[1].union_cases,
            UnionCases::Unknowable,
            "another type's union must keep its cases unknowable"
        );
    }

    // -----------------------------------------------------------------
    // F#-only typar constraints: `apply_typar_constraints`
    // -----------------------------------------------------------------
    //
    // The real FSharp.Core assertions live in the assembly crate's
    // `projector_fsharp_core` group (Map/Set constrained, option/list free).
    // These pin the two ambiguity guards and the measure decline, which no
    // shipped assembly exhibits at the arity-bearing key — an F# source file
    // cannot declare the colliding shapes at all, so a synthetic tree is the
    // only way to reach them.

    /// One typar spec, constrained or not.
    fn typar_spec(name: &str, constrained: bool) -> PickledTyparSpecData {
        PickledTyparSpecData {
            ident: PickledIdent {
                name: name.to_string(),
                range: dummy_range(),
            },
            attribs: Vec::new(),
            flags: 0,
            constraints: if constrained {
                vec![FSharpTyparConstraint::SupportsComparison]
            } else {
                Vec::new()
            },
            xmldoc: PickledXmlDoc { lines: Vec::new() },
        }
    }

    /// A top-level pickled `type` owning typar rows `typars`.
    fn typed_entity_with_typars(logical: &str, typars: Vec<u32>) -> PickledEntity {
        let mut e = make_entity(
            logical,
            measure_object_model_repr(),
            module_modul_typ(vec![]),
        );
        e.typars = typars;
        e
    }

    /// An ECMA row with `arity` generic parameters, every one blanked to
    /// `Unknowable` — the state an F# assembly's rows are in when the overlay
    /// runs.
    /// A class row of `arity` type parameters, named as fsc names one: `name`,
    /// with the arity suffix in its metadata name when it is generic.
    fn ecma_generic(namespace: Vec<&str>, name: &str, arity: usize) -> Entity {
        let mut e = make_ecma_entity(namespace, name, EntityKind::Class);
        if arity > 0 {
            e.metadata_name = Some(format!("{name}`{arity}"));
        }
        e.generic_parameters = (0..arity)
            .map(|i| TypeParameter {
                name: format!("T{i}"),
                variance: crate::model::Variance::Invariant,
                reference_type_constraint: false,
                value_type_constraint: false,
                default_constructor_constraint: false,
                is_unmanaged: false,
                allows_ref_struct: false,
                nullability: crate::model::Nullability::Oblivious,
                type_constraints: Vec::new(),
                fsharp_constraints: FSharpConstraints::Unknowable,
            })
            .collect();
        e
    }

    /// The reading of a [`typar_spec`] written constrained: `comparison` alone.
    const COMPARISON_ONLY: FSharpConstraints = FSharpConstraints::Supports(EqualityComparison {
        equality: false,
        comparison: true,
    });

    fn readings(e: &Entity) -> Vec<FSharpConstraints> {
        e.generic_parameters
            .iter()
            .map(|p| p.fsharp_constraints)
            .collect()
    }

    #[test]
    fn a_pickled_constraint_reaches_its_ecma_typar() {
        // stamps: root(0), N(1), A(2). A generic type's logical name carries
        // its arity suffix, as FCS mangles it.
        let a = typed_entity_with_typars("A`2", vec![0, 1]);
        let [root, ns] = root_and_namespace(vec![2]);
        let mut ccu = make_ccu(vec![root, ns, a], Vec::new(), 0);
        ccu.tables.typars = vec![typar_spec("K", true), typar_spec("V", false)];
        let mut owned = vec![ecma_generic(vec!["N"], "A", 2)];
        apply_typar_constraints(&mut owned, &ccu).expect("apply");
        assert_eq!(
            readings(&owned[0]),
            vec![COMPARISON_ONLY, FSharpConstraints::Free],
            "each typar takes its own pickled reading, by position"
        );
    }

    #[test]
    fn two_pickled_entities_at_one_key_leave_the_row_unknowable() {
        // Two pickled types whose CLR names and arities coincide: neither can
        // say which describes the row, and the row's own name cannot say
        // either.
        let mut a1 = typed_entity_with_typars("A`1", vec![0]);
        a1.compiled_name = Some("A`1".to_string());
        let mut a2 = typed_entity_with_typars("Other`1", vec![1]);
        a2.compiled_name = Some("A`1".to_string());
        let [root, ns] = root_and_namespace(vec![2, 3]);
        let mut ccu = make_ccu(vec![root, ns, a1, a2], Vec::new(), 0);
        ccu.tables.typars = vec![typar_spec("T", true), typar_spec("T", false)];
        let mut owned = vec![ecma_generic(vec!["N"], "A", 1)];
        apply_typar_constraints(&mut owned, &ccu).expect("apply");
        assert_eq!(
            readings(&owned[0]),
            vec![FSharpConstraints::Unknowable],
            "an ambiguous pickle key must not attach whichever entry was walked first"
        );
    }

    #[test]
    fn two_ecma_rows_at_one_key_leave_both_unknowable() {
        // The mirror image: one pickled type, two rows it fits equally.
        let a = typed_entity_with_typars("A", vec![0]);
        let [root, ns] = root_and_namespace(vec![2]);
        let mut ccu = make_ccu(vec![root, ns, a], Vec::new(), 0);
        ccu.tables.typars = vec![typar_spec("T", true)];
        let mut owned = vec![
            ecma_generic(vec!["N"], "A", 1),
            ecma_generic(vec!["N"], "A", 1),
        ];
        apply_typar_constraints(&mut owned, &ccu).expect("apply");
        for row in &owned {
            assert_eq!(
                readings(row),
                vec![FSharpConstraints::Unknowable],
                "two rows at one key: the pickle entry fits each as readily as the other"
            );
        }
    }

    #[test]
    fn a_differing_arity_is_not_the_same_key() {
        // The `List<'T>` / `List` shape FSharp.Core really ships: two pickled
        // entities whose arity-stripped names collide but whose arities do
        // not. Keyed by name alone both would decline; each must find its own
        // row.
        let mut generic = typed_entity_with_typars("A`1", vec![0]);
        generic.compiled_name = Some("A`1".to_string());
        let nullary = typed_entity_with_typars("A", vec![]);
        let [root, ns] = root_and_namespace(vec![2, 3]);
        let mut ccu = make_ccu(vec![root, ns, generic, nullary], Vec::new(), 0);
        ccu.tables.typars = vec![typar_spec("T", true)];
        let mut owned = vec![
            ecma_generic(vec!["N"], "A", 1),
            ecma_generic(vec!["N"], "A", 0),
        ];
        apply_typar_constraints(&mut owned, &ccu).expect("apply");
        assert_eq!(
            readings(&owned[0]),
            vec![COMPARISON_ONLY],
            "the arity-1 row takes the arity-1 pickle entry's reading"
        );
    }

    #[test]
    fn an_alias_never_claims_a_row_it_did_not_synthesise() {
        // The `--standalone` shape: the host pickle declares an erased
        // abbreviation `N.A<'T>`, and the only TypeDef at that key is a copied
        // dependency's — so the marker pass left no marker (the name was
        // taken) and both ambiguity counts are 1. Stamping here would report
        // the foreign type's parameters using the alias's constraints.
        let mut alias = typed_entity_with_typars("A", vec![0]);
        alias.type_abbrev = some_abbrev_target();
        let [root, ns] = root_and_namespace(vec![2]);
        let mut ccu = make_ccu(vec![root, ns, alias], Vec::new(), 0);
        ccu.tables.typars = vec![typar_spec("T", false)];
        let mut owned = vec![ecma_generic(vec!["N"], "A", 1)];
        assert_eq!(owned[0].kind, EntityKind::Class, "the row is a real type");
        apply_typar_constraints(&mut owned, &ccu).expect("apply");
        assert_eq!(
            readings(&owned[0]),
            vec![FSharpConstraints::Unknowable],
            "an erased alias has no row of its own, so a real row at its key is \
             somebody else's"
        );

        // …and the marker it *would* have synthesised does take the reading,
        // so the guard costs an alias nothing where the alias really owns the
        // row.
        let mut marker = ecma_generic(vec!["N"], "A", 1);
        marker.kind = EntityKind::Abbreviation;
        let mut owned = vec![marker];
        apply_typar_constraints(&mut owned, &ccu).expect("apply");
        assert_eq!(
            readings(&owned[0]),
            vec![FSharpConstraints::Free],
            "the alias's own marker is the one row it may claim"
        );
    }

    #[test]
    fn two_aliases_sharing_a_compiled_name_keep_their_own_readings() {
        // `[<CompiledName("X")>] type A<'T when 'T : comparison>` beside
        // `[<CompiledName("X")>] type B<'T>`: legal F#, two distinct source
        // types, and `apply_abbreviation_markers` keeps them apart by source
        // name. Keying by the CLR name alone would collapse them and decline
        // both, throwing away a reading the pickle states outright.
        let mut a = typed_entity_with_typars("A", vec![0]);
        a.compiled_name = Some("X".to_string());
        a.type_abbrev = some_abbrev_target();
        let mut b = typed_entity_with_typars("B", vec![1]);
        b.compiled_name = Some("X".to_string());
        b.type_abbrev = some_abbrev_target();
        let [root, ns] = root_and_namespace(vec![2, 3]);
        let mut ccu = make_ccu(vec![root, ns, a, b], Vec::new(), 0);
        ccu.tables.typars = vec![typar_spec("T", true), typar_spec("T", false)];

        let marker = |source: &str| {
            let mut e = ecma_generic(vec!["N"], "X", 1);
            e.kind = EntityKind::Abbreviation;
            e.source_name = Some(source.to_string());
            e
        };
        let mut owned = vec![marker("A"), marker("B")];
        apply_typar_constraints(&mut owned, &ccu).expect("apply");
        assert_eq!(
            (readings(&owned[0]), readings(&owned[1])),
            (vec![COMPARISON_ONLY], vec![FSharpConstraints::Free]),
            "each alias takes its own pickled reading; the shared compiled name is \
             not their identity"
        );
    }

    #[test]
    fn a_measure_typar_declines_the_whole_entity() {
        // A `[<Measure>]` typar is erased from IL, so the pickled positions do
        // not correspond to the row's generic parameters and nothing may be
        // attached by position.
        let a = typed_entity_with_typars("A", vec![0, 1]);
        let [root, ns] = root_and_namespace(vec![2]);
        let mut ccu = make_ccu(vec![root, ns, a], Vec::new(), 0);
        let mut measure = typar_spec("m", false);
        // `TyparFlags.Kind`: the measure bit, as `is_measure_free` reads it.
        measure.flags = 0b00000000100000000;
        ccu.tables.typars = vec![typar_spec("T", false), measure];
        let mut owned = vec![ecma_generic(vec!["N"], "A", 2)];
        apply_typar_constraints(&mut owned, &ccu).expect("apply");
        assert_eq!(
            readings(&owned[0]),
            vec![FSharpConstraints::Unknowable; 2],
            "a measure-bearing entity's positions do not correspond"
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
    fn each_range_lands_on_the_row_that_bears_its_entitys_compiled_name() {
        // Two pickle entities whose CLR names both strip to `A` (`A`, and
        // `[<CompiledName("A`1")>] B`), and their two rows, which strip alike
        // too. Each row is the one whose metadata name is its entity's compiled
        // name, so each takes its own entity's range. stamps: root(0), N(1),
        // A(2), B(3).
        let a = typed_entity_with_range("A", None, 0, 1, 0);
        let b = typed_entity_with_range("B", Some("A`1"), 0, 2, 0);
        let [root, ns] = root_and_namespace(vec![2, 3]);
        let ccu = ccu_with_strings(vec![root, ns, a, b], vec!["Lib.fs".to_string()]);
        let mut renamed = make_ecma_entity(vec!["N"], "A", EntityKind::Class);
        renamed.metadata_name = Some("A`1".to_string());
        let mut owned = vec![renamed, make_ecma_entity(vec!["N"], "A", EntityKind::Class)];
        apply_entity_overlay(&mut owned, &ccu).unwrap();
        let line = |e: &Entity| e.definition_range.as_ref().map(|r| r.start_line);
        assert_eq!(line(&owned[0]), Some(2), "``A`1`` is B's row");
        assert_eq!(line(&owned[1]), Some(1), "`A` is A's row");

        // With B's row absent, A's row is still A's alone: B names another row.
        let mut owned = vec![make_ecma_entity(vec!["N"], "A", EntityKind::Class)];
        apply_entity_overlay(&mut owned, &ccu).unwrap();
        assert_eq!(line(&owned[0]), Some(1));
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

    /// How a generated val compiled, against what it declares.
    #[derive(Debug, Clone, Copy)]
    enum ValCompiled {
        AsDeclared,
        /// Declared instance; the IL method is static and leads with the
        /// receiver (the generator forces its first parameter to be one).
        StaticWithReceiver,
        /// Declared static, one argument wider than the IL instance method.
        InstanceFromStatic,
        /// Judged receiver-first by the pickle side, wrongly: the IL method is
        /// a static one parameter wider whose first parameter is not the
        /// receiver. `member_compiled` is a port of fsc's rule, and the
        /// receiver check is what stands between a misport and a wrong shape.
        Misjudged,
    }

    /// One generated IL method of `N.T`: its name, parameters over three types
    /// (`int`, `string`, and `N.T` itself) and staticness — its IL identity —
    /// and what the projection made of it, plus the pickled val it compiles
    /// from and how, or nothing for a method the signature file hides.
    #[derive(Debug, Clone)]
    struct ShapeItem {
        name: u8,
        params: Vec<u8>,
        is_static: bool,
        member: ValMember,
        val: Option<ValCompiled>,
    }

    fn shape_item() -> impl Strategy<Value = ShapeItem> {
        let member = prop_oneof![
            Just(ValMember::Kept),
            Just(ValMember::Refused),
            Just(ValMember::Elided),
        ];
        let compiled = prop_oneof![
            Just(ValCompiled::AsDeclared),
            Just(ValCompiled::StaticWithReceiver),
            Just(ValCompiled::InstanceFromStatic),
            Just(ValCompiled::Misjudged),
        ];
        (
            0u8..2,
            proptest::collection::vec(0u8..3, 0..3),
            any::<bool>(),
            member,
            proptest::option::of(compiled),
        )
            .prop_map(|(name, mut params, mut is_static, member, val)| {
                // The IL shape each compilation produces.
                match val {
                    Some(ValCompiled::StaticWithReceiver) => {
                        is_static = true;
                        params.insert(0, 2);
                    }
                    Some(ValCompiled::InstanceFromStatic) => is_static = false,
                    Some(ValCompiled::Misjudged) => {
                        is_static = true;
                        params.insert(0, 0);
                    }
                    Some(ValCompiled::AsDeclared) | None => {}
                }
                ShapeItem {
                    name,
                    params,
                    is_static,
                    member,
                    val,
                }
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// Soundness of `settle_compiled_shapes` against a generated ground
        /// truth. Whatever mix of receiver-first statics, statics compiled
        /// instance, plain members, signature-hidden methods, and refused or
        /// elided projections a type holds — sharing IL slots in any way real
        /// metadata allows — every member left standing has the shape F#
        /// declares for it: one the pass reshaped is a receiver-first val's own
        /// method, now its declared shape, and one it left alone is not the
        /// method of a val compiled under another staticness. And it leaves
        /// alone every member outside the IL slots such vals compile to.
        #[test]
        fn settled_members_have_their_declared_shape(
            items in proptest::collection::vec(shape_item(), 0..8)
        ) {
            let mut seen = std::collections::HashSet::new();
            let items: Vec<ShapeItem> = items
                .into_iter()
                .filter(|i| seen.insert((i.name, i.params.clone(), i.is_static)))
                .collect();
            let receiver = TypeRef::Named {
                assembly: None,
                namespace: vec!["N".to_string()],
                name: "T".to_string(),
                type_args: Vec::new(),
                segment_arities: vec![0],
            };
            let ty = |p: u8| match p {
                0 => TypeRef::Primitive(Primitive::I4),
                1 => TypeRef::Primitive(Primitive::String),
                _ => receiver.clone(),
            };
            let method_of = |i: &ShapeItem, tag: usize| {
                let mut m = make_ecma_method(&format!("M{}", i.name));
                if let Member::Method(mm) = &mut m {
                    mm.is_static = i.is_static;
                    // Tags the member with its item, for the oracle below.
                    mm.metadata_token = tag as u32;
                    mm.signature.parameters = i
                        .params
                        .iter()
                        .map(|p| Parameter {
                            name: None,
                            ty: ty(*p),
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
            };
            let mut entity = make_ecma_entity(vec!["N"], "T", EntityKind::Class);
            let physical: PhysicalMethods = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    (
                        i as u32,
                        PhysicalMethod {
                            name: format!("M{}", item.name),
                            params: Some(item.params.len()),
                            is_static: item.is_static,
                            accessor_of: None,
                        },
                    )
                })
                .collect();
            entity.method_def_tokens = (0..items.len() as u32).collect();
            let mut keys = Vec::new();
            for (tag, item) in items.iter().enumerate() {
                if let Some(compiled) = item.val {
                    let (arity, is_static, compiled) = match compiled {
                        ValCompiled::AsDeclared => {
                            (item.params.len(), item.is_static, Compiled::AsDeclared)
                        }
                        ValCompiled::StaticWithReceiver | ValCompiled::Misjudged => {
                            (item.params.len() - 1, false, Compiled::StaticWithReceiver)
                        }
                        ValCompiled::InstanceFromStatic => {
                            (item.params.len() + 1, true, Compiled::InstanceFromStatic)
                        }
                    };
                    keys.push(DocKey {
                        slot: DocSlot::Method {
                            name: format!("M{}", item.name),
                            arity,
                            is_static,
                            compiled,
                        },
                        key: None,
                        erased_typars: 0,
                        curried: None,
                        role: SlotRole::Plain,
                    });
                }
                match item.member {
                    ValMember::Kept => entity.members.push(method_of(item, tag)),
                    ValMember::Refused => entity.skipped_members.push(SkippedMember {
                        name: format!("M{}", item.name),
                        reason: "refused".to_string(),
                    }),
                    ValMember::Elided => {}
                }
            }
            // The IL slots of the vals compiled under another staticness.
            let lying_slots: Vec<DocSlot> = keys
                .iter()
                .filter(|k| k.slot.compiled() != Compiled::AsDeclared)
                .map(|k| k.slot.compiled_shape())
                .collect();
            let target = TypeMemberTarget {
                namespace: vec!["N".to_string()],
                type_chain: vec!["T".to_string()],
                arity: 0,
                stamp: 0,
                keys,
            };
            let before = entity.members.clone();
            settle_compiled_shapes(&mut entity, &target, &physical);
            for member in &entity.members {
                let Member::Method(m) = member else {
                    unreachable!("the generator makes only methods");
                };
                let item = &items[m.metadata_token as usize];
                let original = method_of(item, m.metadata_token as usize);
                if *member == original {
                    prop_assert!(
                        matches!(item.val, None | Some(ValCompiled::AsDeclared)),
                        "{item:?} kept its IL shape, which F# does not declare"
                    );
                } else {
                    prop_assert!(
                        matches!(item.val, Some(ValCompiled::StaticWithReceiver)),
                        "{item:?} was reshaped, but is not a receiver-first val's method"
                    );
                    prop_assert!(!m.is_static, "{item:?} reshaped to a static");
                    prop_assert_eq!(
                        m.signature.parameters.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
                        item.params[1..].iter().map(|p| ty(*p)).collect::<Vec<_>>(),
                        "{:?} reshaped to the wrong parameters", item
                    );
                }
            }
            for member in &before {
                if !lying_slots.iter().any(|slot| occupies(member, slot)) {
                    prop_assert!(
                        entity.members.contains(member),
                        "{member:?} lies outside every reshaped slot, yet was touched"
                    );
                }
            }
        }
    }

    /// The common case of `settle_compiled_shapes` reshapes rather than
    /// refuses: the property above is satisfied by refusing everything.
    #[test]
    fn a_lone_receiver_first_static_is_reshaped() {
        let receiver = TypeRef::Named {
            assembly: None,
            namespace: vec!["N".to_string()],
            name: "T".to_string(),
            type_args: Vec::new(),
            segment_arities: vec![0],
        };
        let mut m = make_ecma_method("M");
        if let Member::Method(mm) = &mut m {
            mm.is_static = true;
            mm.signature.parameters = [receiver, TypeRef::Primitive(Primitive::I4)]
                .into_iter()
                .map(|ty| Parameter {
                    name: None,
                    ty,
                    is_byref: false,
                    is_out: false,
                    is_readonly_ref: false,
                    default: crate::model::ParamDefault::None,
                    is_param_array: false,
                    nullability: Nullability::Oblivious,
                })
                .collect();
        }
        let mut entity = make_ecma_entity(vec!["N"], "T", EntityKind::Class);
        entity.members.push(m);
        entity.method_def_tokens = vec![0];
        let physical: PhysicalMethods = [(
            0,
            PhysicalMethod {
                name: "M".to_string(),
                params: Some(2),
                is_static: true,
                accessor_of: None,
            },
        )]
        .into_iter()
        .collect();
        let target = TypeMemberTarget {
            namespace: vec!["N".to_string()],
            type_chain: vec!["T".to_string()],
            arity: 0,
            stamp: 0,
            keys: vec![DocKey {
                slot: DocSlot::Method {
                    name: "M".to_string(),
                    arity: 1,
                    is_static: false,
                    compiled: Compiled::StaticWithReceiver,
                },
                key: None,
                erased_typars: 0,
                curried: None,
                role: SlotRole::Plain,
            }],
        };
        settle_compiled_shapes(&mut entity, &target, &physical);
        let [Member::Method(m)] = entity.members.as_slice() else {
            panic!("one method: {:?}", entity.members);
        };
        assert!(!m.is_static);
        assert_eq!(
            m.signature
                .parameters
                .iter()
                .map(|p| &p.ty)
                .collect::<Vec<_>>(),
            [&TypeRef::Primitive(Primitive::I4)]
        );
        assert!(entity.skipped_members.is_empty());
    }

    /// A receiver-first val whose own method the projection elided must not
    /// claim a member of another val that merely has the shape it declares:
    /// after the reshape, `occupies` alone cannot tell a reshaped member from
    /// a natively declared one, and two slots both claiming the one member
    /// let a `HashMap`'s order pick whose argument groups it gets.
    #[test]
    fn an_elided_receiver_first_val_claims_no_other_member() {
        let param = |name: &str| Parameter {
            name: Some(name.to_string()),
            ty: TypeRef::Primitive(Primitive::String),
            is_byref: false,
            is_out: false,
            is_readonly_ref: false,
            default: crate::model::ParamDefault::None,
            is_param_array: false,
            nullability: Nullability::Oblivious,
        };
        let groups = |gs: &[&[&str]]| CurriedArgs {
            leading_receiver: false,
            groups: gs
                .iter()
                .map(|g| ArgGroup {
                    args: g
                        .iter()
                        .map(|n| CurriedArg {
                            name: Some(n.to_string()),
                        })
                        .collect(),
                })
                .collect(),
        };
        let key = |compiled: Compiled, curried: CurriedArgs| DocKey {
            slot: DocSlot::Method {
                name: "M".to_string(),
                arity: 2,
                is_static: false,
                compiled,
            },
            key: None,
            erased_typars: 0,
            curried: Some(curried),
            role: SlotRole::Plain,
        };
        let physical_method = |params: usize, is_static: bool| PhysicalMethod {
            name: "M".to_string(),
            params: Some(params),
            is_static,
            accessor_of: None,
        };
        // HashMap order varies per map, so one run may pass by luck.
        for _ in 0..64 {
            let mut m = make_ecma_method("M");
            if let Member::Method(mm) = &mut m {
                mm.is_static = false;
                mm.signature.parameters = vec![param("x"), param("y")];
            }
            let mut entity = make_ecma_entity(vec!["N"], "T", EntityKind::Record);
            entity.members.push(m);
            entity.method_def_tokens = vec![0, 1];
            // The elided receiver-first method, and the projected one.
            let physical: PhysicalMethods = [
                (0, physical_method(3, true)),
                (1, physical_method(2, false)),
            ]
            .into_iter()
            .collect();
            let target = TypeMemberTarget {
                namespace: vec!["N".to_string()],
                type_chain: vec!["T".to_string()],
                arity: 0,
                stamp: 0,
                keys: vec![
                    key(Compiled::StaticWithReceiver, groups(&[&["a"], &["b"]])),
                    key(Compiled::AsDeclared, groups(&[&["x", "y"]])),
                ],
            };
            overlay_type_row(&mut entity, &target, &physical);
            let [Member::Method(m)] = entity.members.as_slice() else {
                panic!("one method: {:?}", entity.members);
            };
            assert_eq!(
                m.arg_groups,
                ArgGroups::Read(Box::new(groups(&[&["x", "y"]]))),
                "the member keeps its own val's grouping"
            );
        }
    }

    /// [`PickleJoin`] against a model of what fsc writes: every pickled module
    /// or type gets its own `TypeDef` row, named and placed as FCS's
    /// `CompiledRepresentation` and the IL writer's last-`.` split place it;
    /// foreign rows (a `--staticlink` dependency's, a signature-hidden type's)
    /// land anywhere; and the projector drops some rows.
    ///
    /// The names are drawn from an alphabet chosen to collide under every lossy
    /// reading — ``U`` / ``U`0`` / ``U`1`` / ``U`01`` strip alike, ``A.U`` in
    /// namespace `N` is the full name of `U` in namespace `N.A` (and a row the
    /// join must call displaced), `M` and ``M`1`` are module names that contain
    /// namesakes — and each row carries its own index as a tag, so a test can
    /// say whose row a lookup reached.
    mod pickle_join_properties {
        use super::*;
        use crate::ecma335_assembly::strip_arity;

        const NAMESPACES: [&[&str]; 3] = [&[], &["N"], &["N", "A"]];
        const NAMES: [&str; 8] = ["U", "U`0", "U`1", "U`01", "M", "M`1", "A.U", "A"];

        #[derive(Debug, Clone, Copy, PartialEq)]
        enum Kind {
            Module,
            Type,
            /// No row of its own.
            Abbreviation,
        }

        /// One pickled declaration: its namespace, the earlier declaration it
        /// is nested in (honoured only when that is a module of the same
        /// namespace), its compiled name, and its kind.
        #[derive(Debug, Clone)]
        struct Decl {
            ns: usize,
            parent: Option<usize>,
            name: usize,
            kind: Kind,
        }

        /// One `TypeDef` row: its enclosing row, its namespace and name columns,
        /// and whether the projector drops it.
        #[derive(Debug, Clone)]
        struct Row {
            parent: Option<usize>,
            ns_col: String,
            name_col: String,
            dropped: bool,
        }

        /// The IL writer's split of a full type name at its last `.`.
        fn columns(full: &str) -> (String, String) {
            match full.rfind('.') {
                Some(i) => (full[..i].to_string(), full[i + 1..].to_string()),
                None => (String::new(), full.to_string()),
            }
        }

        fn decls() -> impl Strategy<Value = Vec<Decl>> {
            prop::collection::vec(
                (
                    0..NAMESPACES.len(),
                    prop::option::of(0..8usize),
                    0..NAMES.len(),
                    prop_oneof![
                        3 => Just(Kind::Module),
                        3 => Just(Kind::Type),
                        1 => Just(Kind::Abbreviation),
                    ],
                ),
                0..9,
            )
            .prop_map(|raw| {
                let mut out: Vec<Decl> = Vec::new();
                for (ns, parent, name, kind) in raw {
                    let parent = parent.filter(|&p| {
                        p < out.len() && out[p].kind == Kind::Module && out[p].ns == ns
                    });
                    out.push(Decl {
                        ns,
                        parent,
                        name,
                        kind,
                    });
                }
                out
            })
        }

        /// Foreign rows: an enclosing row pick (resolved against the rows that
        /// exist by then), a namespace and a name.
        fn foreign() -> impl Strategy<Value = Vec<(Option<usize>, usize, usize)>> {
            prop::collection::vec(
                (
                    prop::option::of(0..12usize),
                    0..NAMESPACES.len(),
                    0..NAMES.len(),
                ),
                0..4,
            )
        }

        /// Where a declaration's row is, or would be: its enclosing row (its
        /// module's) and its columns.
        fn placement(
            decls: &[Decl],
            own_row: &[Option<usize>],
            i: usize,
        ) -> (Option<usize>, String, String) {
            let d = &decls[i];
            let name = NAMES[d.name];
            match d.parent {
                Some(p) => {
                    let (ns_col, name_col) = columns(name);
                    (own_row[p], ns_col, name_col)
                }
                None => {
                    let ns = NAMESPACES[d.ns].join(".");
                    let full = if ns.is_empty() {
                        name.to_string()
                    } else {
                        format!("{ns}.{name}")
                    };
                    let (ns_col, name_col) = columns(&full);
                    (None, ns_col, name_col)
                }
            }
        }

        fn row_path(rows: &[Row], i: usize) -> RowPath {
            let r = &rows[i];
            match r.parent {
                None => RowPath::top_level(&r.ns_col, &r.name_col),
                Some(p) => row_path(rows, p).nested(&r.ns_col, &r.name_col),
            }
        }

        fn pickled_entity(name: &str, kind: Kind, children: Vec<u32>) -> PickledEntity {
            match kind {
                Kind::Module => {
                    let mut e =
                        make_entity(name, PickledTyconRepr::NoRepr, module_modul_typ(children));
                    e.flags = ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE;
                    e
                }
                Kind::Type => make_entity(
                    name,
                    PickledTyconRepr::Union(Vec::new()),
                    module_modul_typ(Vec::new()),
                ),
                Kind::Abbreviation => make_abbreviation(name),
            }
        }

        /// The pickle: the root (stamp 0), the namespace fragments `N` (1) and
        /// `N.A` (2), then declaration `i` at stamp `3 + i`.
        fn pickle(decls: &[Decl]) -> PickledCcu {
            let stamp = |i: usize| (3 + i) as u32;
            let top_in = |ns: usize| {
                (0..decls.len())
                    .filter(|&i| decls[i].parent.is_none() && decls[i].ns == ns)
                    .map(stamp)
                    .collect::<Vec<_>>()
            };
            let fragment = |name: &str, mut children: Vec<u32>, extra: Vec<u32>| {
                children.extend(extra);
                let mut modul = empty_modul_typ();
                modul.entities = children;
                make_entity(name, PickledTyconRepr::NoRepr, modul)
            };
            let mut tycons = vec![
                fragment("Root", vec![1], top_in(0)),
                fragment("N", vec![2], top_in(1)),
                fragment("A", Vec::new(), top_in(2)),
            ];
            for (i, d) in decls.iter().enumerate() {
                let children = (0..decls.len())
                    .filter(|&j| decls[j].parent == Some(i))
                    .map(stamp)
                    .collect();
                tycons.push(pickled_entity(NAMES[d.name], d.kind, children));
            }
            make_ccu(tycons, Vec::new(), 0)
        }

        /// The projected tree: every row not dropped (nor under a dropped row),
        /// tagged with its own index in `method_def_tokens`.
        fn tree(rows: &[Row]) -> Vec<Entity> {
            fn build(rows: &[Row], parent: Option<usize>) -> Vec<Entity> {
                (0..rows.len())
                    .filter(|&i| rows[i].parent == parent && !rows[i].dropped)
                    .map(|i| {
                        let r = &rows[i];
                        let namespace: Vec<&str> = if r.ns_col.is_empty() {
                            Vec::new()
                        } else {
                            r.ns_col.split('.').collect()
                        };
                        let mut e = make_ecma_entity(
                            namespace,
                            strip_arity(&r.name_col),
                            EntityKind::Class,
                        );
                        e.metadata_name = Some(r.name_col.clone());
                        e.method_def_tokens = vec![i as u32];
                        e.nested_types = build(rows, Some(i));
                        e
                    })
                    .collect()
            }
            build(rows, None)
        }

        fn present(rows: &[Row], i: usize) -> bool {
            !rows[i].dropped && rows[i].parent.is_none_or(|p| present(rows, p))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(4096))]

            /// Every pickled entity reaches exactly its own row, or nothing; it
            /// reaches nothing exactly when its row is shared — with another row
            /// or another entity's path — or dropped; and the join is total
            /// exactly when no row-bearing entity's row is shared.
            #[test]
            fn every_entity_reaches_its_own_row_or_declines(
                decls in decls(),
                foreign in foreign(),
                drops in prop::collection::vec(prop::bool::weighted(0.15), 16),
            ) {
                // Each module or type's own row, in declaration order; a nested
                // declaration's row sits in its module's.
                let mut rows: Vec<Row> = Vec::new();
                let mut own_row: Vec<Option<usize>> = Vec::new();
                for i in 0..decls.len() {
                    if decls[i].kind == Kind::Abbreviation {
                        own_row.push(None);
                        continue;
                    }
                    let (parent, ns_col, name_col) = placement(&decls, &own_row, i);
                    own_row.push(Some(rows.len()));
                    rows.push(Row { parent, ns_col, name_col, dropped: false });
                }
                for (parent, ns, name) in foreign {
                    let parent = parent.filter(|&p| p < rows.len());
                    let full = match parent {
                        Some(_) => NAMES[name].to_string(),
                        None => {
                            let ns = NAMESPACES[ns].join(".");
                            if ns.is_empty() { NAMES[name].to_string() } else { format!("{ns}.{}", NAMES[name]) }
                        }
                    };
                    let (ns_col, name_col) = columns(&full);
                    rows.push(Row { parent, ns_col, name_col, dropped: false });
                }
                for (row, &dropped) in rows.iter_mut().zip(&drops) {
                    row.dropped = dropped;
                }

                let ccu = pickle(&decls);
                let table: Vec<RowPath> = (0..rows.len()).map(|i| row_path(&rows, i)).collect();
                let join = PickleJoin::certify(&ccu, &table).expect("the model pickle walks");
                let mut entities = tree(&rows);

                // The path each declaration claims, row or not.
                let claims: Vec<RowPath> = (0..decls.len())
                    .map(|i| {
                        let (parent, ns_col, name_col) = placement(&decls, &own_row, i);
                        match parent {
                            None => RowPath::top_level(&ns_col, &name_col),
                            Some(p) => row_path(&rows, p).nested(&ns_col, &name_col),
                        }
                    })
                    .collect();
                // Whether no other row shares the identity of row `r` or of any
                // row enclosing it.
                let alone = |r: usize| {
                    let mut at = Some(r);
                    while let Some(a) = at {
                        if table.iter().filter(|p| **p == table[a]).count() != 1 {
                            return false;
                        }
                        at = rows[a].parent;
                    }
                    true
                };
                // Whether a declaration's compiled name, or an enclosing
                // module's, carries a `.`: its row is then not at its F# path.
                fn displaced(decls: &[Decl], i: usize) -> bool {
                    NAMES[decls[i].name].contains('.')
                        || decls[i].parent.is_some_and(|p| displaced(decls, p))
                }
                let mut total = true;
                let joined = join.rows(&entities);
                for i in 0..decls.len() {
                    let stamp = (3 + i) as u32;
                    let reached = joined
                        .entity_mut(&mut entities, stamp)
                        .map(|e| e.method_def_tokens.clone());
                    let Some(r) = own_row[i] else {
                        prop_assert_eq!(reached, None, "an abbreviation reached a row");
                        continue;
                    };
                    let claims_at = claims.iter().filter(|p| **p == claims[i]).count();
                    let unique = alone(r) && claims_at == 1 && !displaced(&decls, i);
                    total &= unique;
                    let expected = (unique && present(&rows, r)).then(|| vec![r as u32]);
                    prop_assert_eq!(
                        reached,
                        expected,
                        "declaration {} ({:?}): alone {}, claims {}",
                        i,
                        &decls[i],
                        alone(r),
                        claims_at
                    );
                }
                prop_assert_eq!(join.is_total(), total);

                // The tree-side identity is the table's.
                let mut seen = Vec::new();
                visit_rows_mut(&mut entities, &mut |path, e| {
                    seen.push((path.cloned(), e.method_def_tokens[0] as usize));
                });
                for (path, tag) in seen {
                    prop_assert_eq!(path.as_ref(), Some(&table[tag]));
                }
            }
        }
    }
}
