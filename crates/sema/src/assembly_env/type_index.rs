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

/// The string-keyed index [`TypeIndex`] replaced, kept beside it under
/// `interning-reference` in a debug build.
#[cfg(all(feature = "interning-reference", debug_assertions))]
#[derive(Debug, Default, Clone)]
struct TextNamespaceTypes {
    all: Vec<EntityHandle>,
    by_source_name: HashMap<String, NamedTypes>,
    by_il_or_source_name: HashMap<String, Vec<EntityHandle>>,
}

/// The top-level types of an env, by namespace and then by name. Under
/// `interning-reference` in a debug build every query is answered by the
/// string-keyed index this replaced too, and the two must agree.
#[derive(Debug, Default, Clone)]
pub(super) struct TypeIndex {
    namespaces: HashMap<PathId, NamespaceTypes>,
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    reference: HashMap<Vec<String>, TextNamespaceTypes>,
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
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        {
            let types = self.reference.entry(namespace.to_vec()).or_default();
            types.all.push(handle);
            for name in std::iter::once(il_name).chain(distinct_source_name) {
                types
                    .by_il_or_source_name
                    .entry(name.to_string())
                    .or_default()
                    .push(handle);
            }
            types
                .by_source_name
                .entry(key_name.to_string())
                .or_default()
                .all
                .push(handle);
        }
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
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        self.reference
            .get_mut(namespace)
            .expect("type namespace was indexed before its first-wins slot")
            .by_source_name
            .get_mut(name)
            .expect("type source name was indexed before its first-wins slot")
            .first_by_arity
            .entry(arity)
            .or_insert(handle);
    }

    fn namespace(&self, names: &NameTable, namespace: PathKey<'_>) -> Option<&NamespaceTypes> {
        namespace.check(names);
        let found = namespace.id.and_then(|id| self.namespaces.get(&id));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        {
            let reference = self.reference.get(namespace.text);
            assert_eq!(
                found.map(|t| &t.all),
                reference.map(|t| &t.all),
                "the type index disagrees with its reference at namespace {:?}",
                namespace.text
            );
        }
        found
    }

    /// The bucket at `(namespace, name)`.
    fn named(
        &self,
        names: &NameTable,
        namespace: PathKey<'_>,
        name: NameKey<'_>,
    ) -> Option<&NamedTypes> {
        name.check(names);
        let found = self
            .namespace(names, namespace)
            .zip(name.id)
            .and_then(|(types, name)| types.by_source_name.get(&name));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.reference
                .get(namespace.text)
                .and_then(|t| t.by_source_name.get(name.text)),
            "the type index disagrees with its reference at {:?}.{:?}",
            namespace.text,
            name.text
        );
        found
    }

    /// Every top-level type at `(namespace, source name)`, in interning
    /// order.
    pub(super) fn types_named(
        &self,
        names: &NameTable,
        namespace: PathKey<'_>,
        name: NameKey<'_>,
    ) -> &[EntityHandle] {
        self.named(names, namespace, name)
            .map_or(&[], |types| types.all.as_slice())
    }

    /// The first-wins type-position slot at `(namespace, source name,
    /// arity)`.
    pub(super) fn first_by_arity(
        &self,
        names: &NameTable,
        namespace: PathKey<'_>,
        name: NameKey<'_>,
        arity: usize,
    ) -> Option<EntityHandle> {
        self.named(names, namespace, name)?
            .first_by_arity
            .get(&arity)
            .copied()
    }

    /// Every top-level type in `namespace`, in interning order.
    pub(super) fn types_in_namespace(
        &self,
        names: &NameTable,
        namespace: PathKey<'_>,
    ) -> &[EntityHandle] {
        self.namespace(names, namespace)
            .map_or(&[], |types| types.all.as_slice())
    }

    /// The handles in `namespace` filed under `name` as an IL or a source
    /// name, in interning order.
    pub(super) fn il_or_source_named(
        &self,
        names: &NameTable,
        namespace: PathKey<'_>,
        name: NameKey<'_>,
    ) -> &[EntityHandle] {
        name.check(names);
        let found = self
            .namespace(names, namespace)
            .zip(name.id)
            .and_then(|(types, name)| types.by_il_or_source_name.get(&name))
            .map_or(&[][..], Vec::as_slice);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.reference
                .get(namespace.text)
                .and_then(|t| t.by_il_or_source_name.get(name.text))
                .map_or(&[][..], Vec::as_slice),
            "the logical-segment index disagrees with its reference at {:?}.{:?}",
            namespace.text,
            name.text
        );
        found
    }

    /// `namespace`'s memo of its fold surfaces, `None` when it holds no
    /// type.
    pub(super) fn fold_memo(
        &self,
        names: &NameTable,
        namespace: PathKey<'_>,
    ) -> Option<&OnceLock<Vec<OpenFoldSurface>>> {
        self.namespace(names, namespace)
            .map(|types| &types.fold_surfaces)
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
        TypeIndex {
            namespaces,
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            reference: self.reference.clone(),
        }
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
