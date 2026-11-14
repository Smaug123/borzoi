//! [`TypeIndex`]: the env's top-level types by namespace and name, keyed by
//! the env's interned paths and names (`docs/sema-interning-plan.md`, Stage
//! 5c).
//!
//! Every namespace and name it holds is interned when it is inserted, which
//! is what seeds the env's [`NameTable`]: a namespace or name the table never
//! interned is in no bucket, so a query whose key has no id answers empty
//! without hashing anything.

use std::sync::OnceLock;

use crate::hash::HashMap;
use crate::names::{Name, NameKey, NameTable, PathId, PathKey};
#[cfg(feature = "test-support")]
use crate::names::{rekey_name, rekey_path};

use super::{EntityHandle, OpenFoldSurface};

/// The top-level entities declared directly in one exact namespace, indexed
/// by the name F# source writes and by either name `seg_name_matches` reads.
/// Every vector keeps interning order and every cross-assembly/arity
/// collision.
#[derive(Debug, Default, Clone)]
pub(super) struct NamespaceTypes {
    /// Every top-level type in the namespace, in interning order.
    pub(super) all: Vec<EntityHandle>,
    by_source_name: HashMap<Name, NamedTypes>,
    /// Each handle in `all` under its IL `name` and, when it differs, under
    /// its `source_name`: the two names `seg_name_matches` compares a
    /// segment, or a mangled segment's base, against. The candidate index
    /// behind `AssemblyEnv::types_at_logical_segment`.
    by_il_or_source_name: HashMap<Name, Vec<EntityHandle>>,
    /// Memo for `AssemblyEnv::open_namespace_fold_surfaces` of this
    /// namespace. Every `mark_*` mutator resets it
    /// (`AssemblyEnv::invalidate_fold_surfaces`).
    pub(super) fold_surfaces: OnceLock<Vec<OpenFoldSurface>>,
}

/// Every top-level entity at one exact `(namespace, source name)`. `all` is
/// the collision-preserving query population; `first_by_arity` is the
/// deterministic type-position slot `AssemblyEnv::lookup_type` reads.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct NamedTypes {
    pub(super) all: Vec<EntityHandle>,
    first_by_arity: HashMap<usize, EntityHandle>,
}

/// The top-level types of an env, by namespace and then by name.
#[derive(Debug, Default, Clone)]
pub(super) struct TypeIndex {
    namespaces: HashMap<PathId, NamespaceTypes>,
}

impl TypeIndex {
    /// File `handle` under `namespace`: in the namespace's population, under
    /// `il_name` and `distinct_source_name` (when there is one) for the
    /// logical-segment index, and under `key_name` (its F# source name) for
    /// the name buckets. Interns the namespace and every name.
    pub(super) fn insert(
        &mut self,
        names: &NameTable,
        namespace: &[String],
        il_name: &str,
        distinct_source_name: Option<&str>,
        key_name: &str,
        handle: EntityHandle,
    ) {
        let types = self
            .namespaces
            .entry(names.intern_path(namespace))
            .or_default();
        types.all.push(handle);
        for name in std::iter::once(il_name).chain(distinct_source_name) {
            types
                .by_il_or_source_name
                .entry(names.logical(name))
                .or_default()
                .push(handle);
        }
        types
            .by_source_name
            .entry(names.logical(key_name))
            .or_default()
            .all
            .push(handle);
    }

    /// Fill the first-wins type-position slot at `(namespace, name, arity)`
    /// with `handle` if it is free. The bucket was filled by
    /// [`Self::insert`] first; a missing one is an invariant violation.
    pub(super) fn fill_first_slot(
        &mut self,
        names: &NameTable,
        namespace: &[String],
        name: &str,
        arity: usize,
        handle: EntityHandle,
    ) {
        let bucket = names
            .lookup_path(namespace)
            .and_then(|id| self.namespaces.get_mut(&id))
            .expect("type namespace was indexed before its first-wins slot")
            .by_source_name
            .get_mut(&names.lookup(name).expect("a type's name was interned"))
            .expect("type source name was indexed before its first-wins slot");
        bucket.first_by_arity.entry(arity).or_insert(handle);
    }

    fn namespace(&self, namespace: PathKey<'_>) -> Option<&NamespaceTypes> {
        namespace.id.and_then(|id| self.namespaces.get(&id))
    }

    /// The bucket at `(namespace, name)`.
    fn named(&self, namespace: PathKey<'_>, name: NameKey<'_>) -> Option<&NamedTypes> {
        self.namespace(namespace)
            .zip(name.id)
            .and_then(|(types, name)| types.by_source_name.get(&name))
    }

    /// Every top-level type at `(namespace, source name)`, in interning
    /// order.
    pub(super) fn types_named(&self, namespace: PathKey<'_>, name: NameKey<'_>) -> &[EntityHandle] {
        self.named(namespace, name)
            .map_or(&[], |types| types.all.as_slice())
    }

    /// The first-wins type-position slot at `(namespace, source name,
    /// arity)`.
    pub(super) fn first_by_arity(
        &self,
        namespace: PathKey<'_>,
        name: NameKey<'_>,
        arity: usize,
    ) -> Option<EntityHandle> {
        self.named(namespace, name)?
            .first_by_arity
            .get(&arity)
            .copied()
    }

    /// Every top-level type in `namespace`, in interning order.
    pub(super) fn types_in_namespace(&self, namespace: PathKey<'_>) -> &[EntityHandle] {
        self.namespace(namespace)
            .map_or(&[], |types| types.all.as_slice())
    }

    /// The handles in `namespace` filed under `name` as an IL or a source
    /// name, in interning order.
    pub(super) fn il_or_source_named(
        &self,
        namespace: PathKey<'_>,
        name: NameKey<'_>,
    ) -> &[EntityHandle] {
        self.namespace(namespace)
            .zip(name.id)
            .and_then(|(types, name)| types.by_il_or_source_name.get(&name))
            .map_or(&[][..], Vec::as_slice)
    }

    /// `namespace`'s memo of its fold surfaces, `None` when it holds no
    /// type.
    pub(super) fn fold_memo(
        &self,
        namespace: PathKey<'_>,
    ) -> Option<&OnceLock<Vec<OpenFoldSurface>>> {
        self.namespace(namespace).map(|types| &types.fold_surfaces)
    }

    /// Drop every namespace's memoised fold surfaces.
    pub(super) fn reset_fold_memos(&mut self) {
        for types in self.namespaces.values_mut() {
            types.fold_surfaces = OnceLock::new();
        }
    }

    /// The same index, keyed by `to`'s ids, its memos dropped.
    #[cfg(feature = "test-support")]
    pub(super) fn rekey(&self, from: &NameTable, to: &NameTable) -> Self {
        let namespaces = self
            .namespaces
            .iter()
            .map(|(&id, types)| {
                (
                    rekey_path(from, to, id),
                    NamespaceTypes {
                        all: types.all.clone(),
                        by_source_name: rekey_names(from, to, &types.by_source_name),
                        by_il_or_source_name: rekey_names(from, to, &types.by_il_or_source_name),
                        fold_surfaces: OnceLock::new(),
                    },
                )
            })
            .collect();
        TypeIndex { namespaces }
    }
}

/// `map` with each key re-keyed from `from`'s ids to `to`'s.
#[cfg(feature = "test-support")]
fn rekey_names<V: Clone>(
    from: &NameTable,
    to: &NameTable,
    map: &HashMap<Name, V>,
) -> HashMap<Name, V> {
    map.iter()
        .map(|(&name, v)| (rekey_name(from, to, name), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// One namespace's types keyed by text: the index a [`TypeIndex`] is
    /// held to, built from the same inserts.
    #[derive(Default)]
    struct TextNamespaceTypes {
        all: Vec<EntityHandle>,
        by_source_name: HashMap<String, (Vec<EntityHandle>, HashMap<usize, EntityHandle>)>,
        by_il_or_source_name: HashMap<String, Vec<EntityHandle>>,
    }

    /// A small alphabet, so namespaces, names and their prefixes collide.
    fn name() -> impl Strategy<Value = String> {
        prop::sample::select(vec!["A", "B", "op_Addition", "C`1"]).prop_map(str::to_string)
    }

    fn namespace() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(name(), 0..3)
    }

    /// One type filed in the index: its namespace, IL name, distinct
    /// source name, key name and arity.
    fn insert() -> impl Strategy<Value = (Vec<String>, String, Option<String>, String, usize)> {
        (
            namespace(),
            name(),
            prop::option::of(name()),
            name(),
            0..3usize,
        )
    }

    proptest! {
        /// Every query of the index answers as the same query of the
        /// string-keyed index built from the same inserts, by keys looked
        /// up in a table that also holds decoy paths and names, and after
        /// re-keying onto another table.
        #[test]
        fn the_type_index_answers_as_its_text_does(
            decoys in prop::collection::vec(namespace(), 0..4),
            inserts in prop::collection::vec(insert(), 0..16),
            namespaces in prop::collection::vec(namespace(), 0..6),
            probes in prop::collection::vec(name(), 0..6),
        ) {
            let names = NameTable::default();
            for decoy in &decoys {
                names.intern_path(&decoy.iter().rev().cloned().collect::<Vec<_>>());
            }
            let mut index = TypeIndex::default();
            let mut model: HashMap<Vec<String>, TextNamespaceTypes> = HashMap::default();
            for (i, (ns, il_name, source_name, key_name, arity)) in inserts.iter().enumerate() {
                let handle = EntityHandle::new(i);
                let distinct = source_name.as_deref().filter(|s| s != il_name);
                index.insert(&names, ns, il_name, distinct, key_name, handle);
                index.fill_first_slot(&names, ns, key_name, *arity, handle);
                let types = model.entry(ns.clone()).or_default();
                types.all.push(handle);
                for n in std::iter::once(il_name.as_str()).chain(distinct) {
                    types.by_il_or_source_name.entry(n.to_string()).or_default().push(handle);
                }
                let (all, first) = types.by_source_name.entry(key_name.clone()).or_default();
                all.push(handle);
                first.entry(*arity).or_insert(handle);
            }
            let fresh = NameTable::default();
            for decoy in decoys.iter().rev() {
                fresh.intern_path(decoy);
            }
            let rekeyed = index.rekey(&names, &fresh);
            let asked: Vec<&Vec<String>> = namespaces
                .iter()
                .chain(inserts.iter().map(|(ns, ..)| ns))
                .chain(&decoys)
                .collect();
            let names_asked: Vec<&String> = probes
                .iter()
                .chain(inserts.iter().flat_map(|(_, il, source, key, _)| {
                    std::iter::once(il).chain(source).chain(std::iter::once(key))
                }))
                .collect();
            for (table, index) in [(&names, &index), (&fresh, &rekeyed)] {
                for &ns in &asked {
                    let types = model.get(ns);
                    let key = PathKey::of(table, ns);
                    prop_assert_eq!(
                        index.types_in_namespace(key),
                        types.map_or(&[][..], |t| t.all.as_slice()),
                        "{:?}", ns
                    );
                    prop_assert_eq!(index.fold_memo(key).is_some(), types.is_some(), "{:?}", ns);
                    for &n in &names_asked {
                        let name = NameKey::of(table, n);
                        let named = types.and_then(|t| t.by_source_name.get(n));
                        prop_assert_eq!(
                            index.types_named(key, name),
                            named.map_or(&[][..], |(all, _)| all.as_slice()),
                            "{:?}.{:?}", ns, n
                        );
                        for arity in 0..3 {
                            prop_assert_eq!(
                                index.first_by_arity(key, name, arity),
                                named.and_then(|(_, first)| first.get(&arity).copied()),
                                "{:?}.{:?}`{}", ns, n, arity
                            );
                        }
                        prop_assert_eq!(
                            index.il_or_source_named(key, name),
                            types
                                .and_then(|t| t.by_il_or_source_name.get(n))
                                .map_or(&[][..], Vec::as_slice),
                            "{:?}.{:?}", ns, n
                        );
                    }
                }
            }
        }
    }
}
