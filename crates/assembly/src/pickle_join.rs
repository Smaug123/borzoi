//! Which metadata `TypeDef` row each entity of a host F# signature pickle
//! compiled to — the one join every pickle overlay in
//! [`crate::fsharp_pickle_merge`] reaches a row through.
//!
//! ## The identity
//!
//! FCS names a type's compiled row from the type itself
//! (`Entity.CompiledRepresentation`, `TypedTree.fs`): the row's name is the
//! entity's `CompiledName` — its `[<CompiledName>]` when it has one, else its
//! logical name, which already carries the backtick arity suffix when the type
//! has non-erased type parameters — and its position is the entity's
//! compilation path, whose namespace fragments join into the outermost row's
//! full name and whose modules become enclosing rows. Signature conformance
//! requires a signature and its implementation to agree on `CompiledName`
//! (`SignatureConformance.fs`), so the pickle, which records the signature,
//! names the very row the implementation emitted.
//!
//! So a row's identity here is a [`RowPath`]: the full name of each row from
//! the outermost enclosing one in, where a row's full name is its namespace and
//! name columns joined by `.` — the IL writer (`ilwrite.fs`
//! `GetTypeNameAsElemPair`) splits every type name, nested ones included, into
//! the two columns at its last `.`, so the join is what puts it back. Nothing
//! is stripped or counted: a row named ``U`0`` and one named `U` are different
//! rows, as are ``M`1`` and `M` containing a namesake, and a
//! `[<CompiledName("A.B")>]` type is found in namespace `….A`.
//!
//! ## What the join certifies
//!
//! A pickled entity is [`JoinVerdict::Joined`] to a row only when the
//! correspondence is one-to-one over the *whole* `TypeDef` table — every row
//! the image declares, including the ones the projector dropped or skips — and
//! over every pickled entity that could claim a row:
//!
//! - the entity is one fsc certainly emits a `TypeDef` for
//!   ([`has_own_type_def`]); an abbreviation, say, has none, so the row at its
//!   path is somebody else's;
//! - no other pickled entity claims the same path; and
//! - exactly one row of the table has the path, and exactly one has each
//!   enclosing path — a row under one of two rows named `M` is reached through
//!   a name that does not say which `M`.
//!
//! Counting the dropped rows is what makes a dropped own row decline rather
//! than hand its reading to a surviving squatter, and counting every claimant
//! (abbreviations and other row-less entities included) is the conservative
//! direction: an extra claimant can only cost a join, never misplace one.
//!
//! [`JoinedRows::entity_mut`] then walks the projected tree to the joined row
//! by the same identity, reading each projected row's raw name off
//! [`Entity::metadata_name`]. A row the projector dropped is simply not found:
//! the overlay declines.
//!
//! ## Totality
//!
//! A declined join is only a decline if nothing reads the row it would have
//! reached as complete. Several overlays write facts whose absence a consumer
//! takes at face value — a module's extension-member names, an abbreviation's
//! marker, a module's source name — so a row the pickle describes but the join
//! cannot attribute would read as "no extensions, no abbreviation, this name".
//! [`PickleJoin::is_total`] is therefore part of what makes a host pickle
//! authoritative: an image with any such row is treated like one whose pickle
//! does not describe it, which every consumer already handles.

use std::collections::HashMap;

use crate::error::ImportError;
use crate::fsharp_pickle::model::{
    IsType, PickledCcu, PickledEntity, PickledExnRepr, PickledTyconRepr,
};
use crate::model::Entity;

/// The `EntityFlags.IsModuleOrNamespace` bit (`TypedTree.fs` `EntityFlags`).
/// Verified empirically against real fixture pickles: **set** for every F#
/// module (plain and `…WithSuffix`) and every namespace, **clear** for every
/// type — union, record, class, struct, exception, `[<Measure>]`, an
/// abbreviation, and a signature-hidden union. So `flags & this == 0`
/// positively identifies a *type* node, distinct from a module / namespace.
pub(crate) const ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE: i64 = 0b1;

/// The exact metadata identity of a `TypeDef` row. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RowPath(Vec<String>);

impl RowPath {
    /// A top-level row, from its namespace and name columns verbatim.
    pub(crate) fn top_level(namespace: &str, name: &str) -> Self {
        RowPath(vec![full_name(namespace, name)])
    }

    /// The row nested directly in `self` with these namespace and name
    /// columns.
    pub(crate) fn nested(&self, namespace: &str, name: &str) -> Self {
        self.nested_full(full_name(namespace, name))
    }

    /// The row nested directly in `self` whose full name is `full`.
    fn nested_full(&self, full: String) -> Self {
        let mut segments = self.0.clone();
        segments.push(full);
        RowPath(segments)
    }

    /// The full names of the rows from the outermost in.
    pub(crate) fn segments(&self) -> &[String] {
        &self.0
    }

    /// The identity of a projected **top-level** row, or `None` for an entity
    /// no row stands behind ([`Entity::metadata_name`]).
    pub(crate) fn of_top_level(entity: &Entity) -> Option<Self> {
        Some(RowPath(vec![projected_full_name(entity)?]))
    }

    /// The identity of `child`, a projected row nested directly in the row at
    /// `self`, or `None` for an entity no row stands behind.
    pub(crate) fn child(&self, child: &Entity) -> Option<Self> {
        Some(self.nested_full(projected_full_name(child)?))
    }
}

/// `namespace.name`, or `name` alone in an empty namespace column.
fn full_name(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}.{name}")
    }
}

/// A projected row's full name, from the namespace the projector split off
/// the row's namespace column and the row's [`Entity::metadata_name`].
fn projected_full_name(entity: &Entity) -> Option<String> {
    Some(full_name(
        &entity.namespace.join("."),
        entity.metadata_name.as_deref()?,
    ))
}

/// Why a pickled entity is or is not joined to a row. Every non-namespace
/// entity of the pickle gets exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JoinVerdict {
    /// One row, one claimant: the row is this entity's own.
    Joined,
    /// fsc emits no `TypeDef` for this entity (an abbreviation, a measure
    /// abbreviation, an inline-IL representation, an exception abbreviation),
    /// so a row at its path is not its own.
    NoOwnTypeDef,
    /// Another pickled entity claims the same path.
    SharedWithAnotherEntity,
    /// No row of the table has this path.
    NoRow,
    /// More than one row of the table has this path.
    SharedWithAnotherRow,
}

/// Where a pickled entity's row sits: at the top level of a namespace, or
/// nested in the row of an enclosing module.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Container {
    /// The namespace, as its dotted components (`[]` for the global one).
    Namespace(Vec<String>),
    /// The stamp of the enclosing module.
    Entity(u32),
}

#[derive(Debug, Clone)]
struct Site {
    path: RowPath,
    container: Container,
    verdict: JoinVerdict,
}

/// The certified join of one host pickle against one image's `TypeDef` table.
/// Built once per projection by [`PickleJoin::certify`] and handed to every
/// overlay.
#[derive(Debug, Clone, Default)]
pub(crate) struct PickleJoin {
    sites: HashMap<u32, Site>,
}

impl PickleJoin {
    /// Join every non-namespace entity of `pickled` against `rows`, the
    /// identity of every `TypeDef` row of the image (in any order, dropped and
    /// skipped rows included).
    pub(crate) fn certify(pickled: &PickledCcu, rows: &[RowPath]) -> Result<Self, ImportError> {
        let mut claimants: Vec<(u32, bool, RowPath, Container)> = Vec::new();
        walk(
            pickled,
            pickled.root_entity,
            Walk::Root,
            &mut Vec::new(),
            &mut claimants,
        )?;
        let mut row_counts: HashMap<&RowPath, usize> = HashMap::new();
        for row in rows {
            *row_counts.entry(row).or_default() += 1;
        }
        let mut claim_counts: HashMap<&RowPath, usize> = HashMap::new();
        for (_, _, path, _) in &claimants {
            *claim_counts.entry(path).or_default() += 1;
        }
        let mut sites = HashMap::new();
        for (stamp, own_row, path, container) in &claimants {
            let verdict = if !own_row {
                JoinVerdict::NoOwnTypeDef
            } else if claim_counts[path] > 1 {
                JoinVerdict::SharedWithAnotherEntity
            } else if !row_counts.contains_key(path) {
                JoinVerdict::NoRow
            } else if (1..=path.0.len())
                .any(|k| row_counts.get(&RowPath(path.0[..k].to_vec())) != Some(&1))
            {
                // The row itself is shared, or an enclosing row is: with two
                // rows at `M`, the one row at `M/X` is still reached through a
                // name that does not say which `M` it is under.
                JoinVerdict::SharedWithAnotherRow
            } else {
                JoinVerdict::Joined
            };
            sites.insert(
                *stamp,
                Site {
                    path: path.clone(),
                    container: container.clone(),
                    verdict,
                },
            );
        }
        Ok(PickleJoin { sites })
    }

    /// The verdict for the entity at `stamp`; `None` for the root, a
    /// namespace, or a stamp the walk never reached.
    pub(crate) fn verdict(&self, stamp: u32) -> Option<JoinVerdict> {
        self.sites.get(&stamp).map(|s| s.verdict)
    }

    /// The joined row's identity, when the entity at `stamp` is joined.
    pub(crate) fn joined_path(&self, stamp: u32) -> Option<&RowPath> {
        self.sites
            .get(&stamp)
            .filter(|s| s.verdict == JoinVerdict::Joined)
            .map(|s| &s.path)
    }

    /// Where the row of the entity at `stamp` sits — joined or not, since a
    /// marker for a row-less entity is hung in the same place.
    pub(crate) fn container(&self, stamp: u32) -> Option<&Container> {
        self.sites.get(&stamp).map(|s| &s.container)
    }

    /// Index the projected tree `entities` for lookups by stamp
    /// ([`JoinedRows`]): one walk, so an overlay pays for its lookups in
    /// proportion to its targets rather than to the tree's width.
    pub(crate) fn rows(&self, entities: &[Entity]) -> JoinedRows<'_> {
        JoinedRows {
            join: self,
            positions: positions(entities),
        }
    }

    /// Whether every entity fsc emits a row for is joined to that row, so that
    /// no row the pickle describes is left un-overlaid (see "Totality" in the
    /// module docs).
    pub(crate) fn is_total(&self) -> bool {
        self.sites
            .values()
            .all(|s| matches!(s.verdict, JoinVerdict::Joined | JoinVerdict::NoOwnTypeDef))
    }

    /// How many entities got each verdict.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn census(&self) -> JoinCensus {
        let mut census = JoinCensus::default();
        for site in self.sites.values() {
            *census.verdicts.entry(site.verdict).or_default() += 1;
        }
        census
    }
}

/// A [`PickleJoin`] with the position of every projected row it can reach, as
/// [`PickleJoin::rows`] indexed them.
///
/// The positions are those of the tree as indexed. Every lookup re-checks the
/// identity of each row on the way down, so a tree reordered since answers
/// `None` rather than another row; adding rows at the end of a list, as the
/// marker pass does, moves no position.
pub(crate) struct JoinedRows<'j> {
    join: &'j PickleJoin,
    positions: HashMap<RowPath, Vec<usize>>,
}

impl JoinedRows<'_> {
    /// The projected row the entity at `stamp` compiled to, or `None` when the
    /// entity is not [`JoinVerdict::Joined`] or its row is not in the projected
    /// tree (the projector dropped it).
    pub(crate) fn entity_mut<'a>(
        &self,
        entities: &'a mut [Entity],
        stamp: u32,
    ) -> Option<&'a mut Entity> {
        let path = self.join.joined_path(stamp)?;
        let position = self.positions.get(path)?;
        let (&first, rest) = position.split_first()?;
        let mut current = entities.get_mut(first)?;
        let mut segments = path.segments().iter();
        if !full_name_is(current, segments.next()?) {
            return None;
        }
        for (&i, segment) in rest.iter().zip(segments) {
            current = current.nested_types.get_mut(i)?;
            if !full_name_is(current, segment) {
                return None;
            }
        }
        Some(current)
    }

    /// The rows directly in the container of the entity at `stamp`, as the
    /// list a row would be added to: the top-level list for a namespace-level
    /// entity (whose rows the caller filters by `namespace`), or the enclosing
    /// module row's nested types. `None` when the enclosing module's row cannot
    /// be reached.
    pub(crate) fn container_rows_mut<'a>(
        &self,
        entities: &'a mut Vec<Entity>,
        stamp: u32,
    ) -> Option<ContainerRows<'a>> {
        match self.join.container(stamp)? {
            Container::Namespace(namespace) => Some(ContainerRows {
                namespace: Some(namespace.clone()),
                rows: entities,
            }),
            Container::Entity(module) => Some(ContainerRows {
                namespace: None,
                rows: &mut self.entity_mut(entities, *module)?.nested_types,
            }),
        }
    }
}

/// Whether a projected row's full name is `full`, compared without building
/// it.
fn full_name_is(entity: &Entity, full: &str) -> bool {
    let Some(name) = entity.metadata_name.as_deref() else {
        return false;
    };
    let mut rest = full;
    for part in &entity.namespace {
        match rest
            .strip_prefix(part.as_str())
            .and_then(|r| r.strip_prefix('.'))
        {
            Some(r) => rest = r,
            None => return false,
        }
    }
    rest == name
}

/// The position of every projected row a lookup may reach: one whose identity,
/// and each enclosing row's, no other projected row shares.
///
/// The join certified that the `TypeDef` table holds exactly one row at a
/// joined path and at each of its prefixes, and every projected row reads back
/// as its own row's identity, so the tree cannot hold two. The count is checked
/// anyway: it costs nothing over the walk, and a tree assembled some other way
/// (a test's) is then held to the same answer.
fn positions(entities: &[Entity]) -> HashMap<RowPath, Vec<usize>> {
    fn walk(
        entities: &[Entity],
        parent: Option<(&RowPath, &[usize])>,
        out: &mut Vec<(RowPath, Vec<usize>)>,
    ) {
        for (i, e) in entities.iter().enumerate() {
            let (path, position) = match parent {
                None => (RowPath::of_top_level(e), vec![i]),
                Some((p, pos)) => (p.child(e), [pos, &[i]].concat()),
            };
            if let Some(path) = path {
                walk(&e.nested_types, Some((&path, &position)), out);
                out.push((path, position));
            }
        }
    }
    let mut all = Vec::new();
    walk(entities, None, &mut all);
    let mut counts: HashMap<&RowPath, usize> = HashMap::new();
    for (path, _) in &all {
        *counts.entry(path).or_default() += 1;
    }
    let alone = |path: &RowPath| {
        (1..=path.0.len()).all(|k| counts.get(&RowPath(path.0[..k].to_vec())) == Some(&1))
    };
    let keep: Vec<bool> = all.iter().map(|(path, _)| alone(path)).collect();
    all.into_iter()
        .zip(keep)
        .filter_map(|(entry, keep)| keep.then_some(entry))
        .collect()
}

/// The rows of one container, as [`JoinedRows::container_rows_mut`] returns
/// them.
pub(crate) struct ContainerRows<'a> {
    /// `Some(namespace)` when `rows` is the top-level list, of which only the
    /// rows in `namespace` belong to the container; `None` when `rows` is a
    /// module's nested types, all of which do.
    pub(crate) namespace: Option<Vec<String>>,
    pub(crate) rows: &'a mut Vec<Entity>,
}

impl ContainerRows<'_> {
    /// Whether `row`, an element of [`Self::rows`], is in the container.
    pub(crate) fn contains(&self, row: &Entity) -> bool {
        self.namespace
            .as_ref()
            .is_none_or(|ns| row.namespace == *ns)
    }
}

/// How many pickled entities got each [`JoinVerdict`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JoinCensus {
    pub verdicts: std::collections::BTreeMap<JoinVerdict, usize>,
}

/// Visit every projected entity with its row's identity — `None` for an entity
/// no row stands behind (an abbreviation marker). The tree-side counterpart of
/// [`PickleJoin::joined_path`], for an overlay that must visit every row (to
/// settle the ones the pickle does not describe too).
pub(crate) fn visit_rows_mut(
    entities: &mut [Entity],
    visit: &mut impl FnMut(Option<&RowPath>, &mut Entity),
) {
    fn go(
        entities: &mut [Entity],
        parent: Option<Option<&RowPath>>,
        visit: &mut impl FnMut(Option<&RowPath>, &mut Entity),
    ) {
        for e in entities {
            let path = match parent {
                None => RowPath::of_top_level(e),
                Some(Some(p)) => p.child(e),
                Some(None) => None,
            };
            visit(path.as_ref(), e);
            go(&mut e.nested_types, Some(path.as_ref()), visit);
        }
    }
    go(entities, None, visit);
}

/// Whether fsc certainly emits a `TypeDef` of its own for this pickled
/// (non-namespace) entity, at the entity's compiled path.
///
/// A positive domain, read off FCS's code generator (`IlxGen.fs`
/// `GenTypeDef` / `GenExnDef`, and the module classes): a module; an exception
/// with a fresh representation; and a type that is neither an abbreviation nor
/// an inline-IL (`AsmRepr`) or measure-abbreviation (`Measureable`)
/// representation. A type whose representation the signature hides pickles
/// `NoRepr`, and is still in the domain: conformance rejects a signature that
/// hides an inline-IL or measure-abbreviation representation, and an
/// implementation's abbreviation must be revealed, so behind `NoRepr` there is
/// always an F# type definition, which `GenTypeDef` emits.
pub(crate) fn has_own_type_def(entity: &PickledEntity) -> bool {
    if entity.flags & ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE != 0 {
        // A namespace is never a claimant; what is left is a module.
        return entity.module_type.is_type != IsType::Namespace;
    }
    match entity.exn_repr {
        PickledExnRepr::Fresh(_) => return true,
        PickledExnRepr::Abbrev(_) | PickledExnRepr::Asm(_) => return false,
        PickledExnRepr::None => {}
    }
    entity.type_abbrev.is_none()
        && match entity.repr {
            PickledTyconRepr::NoRepr
            | PickledTyconRepr::Record(_)
            | PickledTyconRepr::Union(_)
            | PickledTyconRepr::UnionWithStaticFields { .. }
            | PickledTyconRepr::FSharpObjectModel(_) => true,
            PickledTyconRepr::AsmRepr(_) | PickledTyconRepr::Measureable(_) => false,
        }
}

/// Whether a pickled entity is a namespace fragment, which owns no row and so
/// claims none.
fn is_namespace(entity: &PickledEntity) -> bool {
    entity.flags & ENTITY_FLAGS_IS_MODULE_OR_NAMESPACE != 0
        && entity.module_type.is_type == IsType::Namespace
}

/// The compiled position the walk has reached.
#[derive(Clone)]
enum Walk {
    /// The synthetic CCU root, which contributes no name.
    Root,
    /// Inside a namespace (its fragments so far, outermost first) and no module.
    Namespace(Vec<String>),
    /// Inside the module at `stamp`, whose row is at `path`.
    Module { stamp: u32, path: RowPath },
}

/// Collect `(stamp, has_own_type_def, path, container)` for every claimant
/// under `stamp`, depth-first.
///
/// The cycle guard is [`crate::fsharp_pickle_merge`]'s: a corrupt pickle can
/// link an ancestor as its own descendant, and the walk runs on the caller's
/// stack, so a repeat on the current descent is an error rather than an
/// overflow.
fn walk(
    pickled: &PickledCcu,
    stamp: u32,
    at: Walk,
    descent: &mut Vec<u32>,
    out: &mut Vec<(u32, bool, RowPath, Container)>,
) -> Result<(), ImportError> {
    if descent.contains(&stamp) {
        return Err(ImportError::PickleEntityCycle { stamp });
    }
    let entity =
        pickled
            .tables
            .tycons
            .get(stamp as usize)
            .ok_or(ImportError::OsgnIndexOutOfRange {
                kind: "tycon (pickle join)",
                index: stamp,
                max: pickled.tables.tycons.len(),
            })?;
    let inner = match &at {
        Walk::Root => Walk::Namespace(Vec::new()),
        _ if is_namespace(entity) => match &at {
            Walk::Namespace(ns) => {
                let mut ns = ns.clone();
                ns.push(entity.logical_name.clone());
                Walk::Namespace(ns)
            }
            // A namespace inside a module is no shape FCS builds; nothing
            // under it can be placed.
            _ => return Ok(()),
        },
        _ => {
            let name = entity
                .compiled_name
                .as_deref()
                .unwrap_or(&entity.logical_name);
            let (path, container) = match &at {
                Walk::Namespace(ns) => (
                    RowPath::top_level(&ns.join("."), name),
                    Container::Namespace(ns.clone()),
                ),
                Walk::Module { stamp, path } => (
                    path.nested_full(name.to_string()),
                    Container::Entity(*stamp),
                ),
                Walk::Root => unreachable!("the root arm is matched first"),
            };
            out.push((stamp, has_own_type_def(entity), path.clone(), container));
            Walk::Module { stamp, path }
        }
    };
    descent.push(stamp);
    for &child in &entity.module_type.entities {
        walk(pickled, child, inner.clone(), descent, out)?;
    }
    descent.pop();
    Ok(())
}
