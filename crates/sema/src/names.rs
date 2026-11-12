//! The name table: interned identifiers ([`Name`]) and paths ([`PathId`]),
//! one table per [`AssemblyEnv`](crate::AssemblyEnv)
//! (`docs/sema-interning-plan.md`, D2 and D3).
//!
//! The table only grows. An id is an index into the table that issued it, so
//! it means something only next to that table's env, which is the rule an
//! [`EntityHandle`](crate::EntityHandle) already follows: every sema result
//! the LSP keeps is paired with the env it was computed against.
//!
//! Ids are interned in whatever order the walks meet names, which differs
//! between runs, so they never order anything: [`Name`] and [`PathId`]
//! implement `Eq` and `Hash` and not `Ord`. In a debug build each id also
//! carries its table's tag, and comparing ids from two tables, or resolving
//! one against the wrong table, panics.
//!
//! Reads by id take no lock: texts and path nodes sit in [`AppendOnly`]
//! storage, whose slots are written once. Interning and the text → id and
//! `(parent, name)` → child probes go through one [`RwLock`]ed index.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{OnceLock, PoisonError, RwLock};

use crate::hash::HashMap;

/// One identifier in F#'s *logical* domain: the text `id_text` produces,
/// backticks stripped and a bare operator compiled. Interned by
/// [`NameTable::logical`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Name {
    index: u32,
    #[cfg(debug_assertions)]
    table: u32,
}

/// A node in the table's trie of [`Name`]s: a path, as its parent path and
/// its last name. The root is the empty path ([`NameTable::root`]).
///
/// A `PathId` is a spelling, not an entity: one path can name a namespace, a
/// module and a type at once, and which one it denotes is decided by the map
/// it is looked up in.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PathId {
    index: u32,
    #[cfg(debug_assertions)]
    table: u32,
}

macro_rules! id_impls {
    ($ty:ident) => {
        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                #[cfg(debug_assertions)]
                assert_eq!(
                    self.table, other.table,
                    concat!(
                        "two ",
                        stringify!($ty),
                        "s from different name tables compared"
                    )
                );
                self.index == other.index
            }
        }

        impl Eq for $ty {}

        impl std::hash::Hash for $ty {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.index.hash(state);
            }
        }
    };
}

id_impls!(Name);
id_impls!(PathId);

/// Where a path node's parent and last name are kept.
#[derive(Clone, Copy, Debug)]
struct PathNode {
    parent: PathId,
    last: Name,
}

/// Storage that only grows, read by index without a lock. Slot `i` sits in
/// chunk `k`, which holds `FIRST_CHUNK << k` slots, so no slot ever moves
/// once written and a reader holding `&self` can borrow it.
struct AppendOnly<T> {
    chunks: [OnceLock<Box<[OnceLock<T>]>>; CHUNKS],
}

/// The first chunk's size.
const FIRST_CHUNK: usize = 32;
/// Enough chunks for every `u32` index: they hold `FIRST_CHUNK × (2^CHUNKS −
/// 1)` slots in all, at least `2^32`.
const CHUNKS: usize = 28;

impl<T> Default for AppendOnly<T> {
    fn default() -> Self {
        AppendOnly {
            chunks: std::array::from_fn(|_| OnceLock::new()),
        }
    }
}

impl<T> AppendOnly<T> {
    /// Slot `index`'s chunk and its offset in it.
    fn locate(index: u32) -> (usize, usize) {
        let index = index as usize;
        let biased = index / FIRST_CHUNK + 1;
        let chunk = (usize::BITS - 1 - biased.leading_zeros()) as usize;
        let start = FIRST_CHUNK * ((1 << chunk) - 1);
        (chunk, index - start)
    }

    fn get(&self, index: u32) -> Option<&T> {
        let (chunk, offset) = Self::locate(index);
        self.chunks[chunk].get()?.get(offset)?.get()
    }

    /// Write slot `index`, which must be empty.
    fn set(&self, index: u32, value: T) {
        let (chunk, offset) = Self::locate(index);
        let slots = self.chunks[chunk]
            .get_or_init(|| (0..FIRST_CHUNK << chunk).map(|_| OnceLock::new()).collect());
        assert!(
            slots[offset].set(value).is_ok(),
            "a name table slot is written once"
        );
    }
}

/// The probes that need hashing: text → name, and `(parent, name)` → child.
#[derive(Default)]
struct Index {
    names: HashMap<Box<str>, u32>,
    children: HashMap<(u32, u32), u32>,
    /// How many names have been interned: the next name's index.
    name_count: u32,
    /// How many paths have been interned, the root included: the next path's
    /// index.
    path_count: u32,
}

/// The interned names and paths of one env. See the module docs.
pub(crate) struct NameTable {
    /// This table's tag, unique in the process: what a debug build's ids
    /// carry.
    tag: u32,
    texts: AppendOnly<Box<str>>,
    /// Path `i`'s node at slot `i − 1`; the root has none.
    paths: AppendOnly<PathNode>,
    index: RwLock<Index>,
}

impl Default for NameTable {
    fn default() -> Self {
        static NEXT_TAG: AtomicU32 = AtomicU32::new(0);
        NameTable {
            tag: NEXT_TAG.fetch_add(1, Ordering::Relaxed),
            texts: AppendOnly::default(),
            paths: AppendOnly::default(),
            index: RwLock::new(Index {
                path_count: 1,
                ..Index::default()
            }),
        }
    }
}

impl std::fmt::Debug for NameTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let index = self.read();
        f.debug_struct("NameTable")
            .field("tag", &self.tag)
            .field("names", &index.name_count)
            .field("paths", &index.path_count)
            .finish()
    }
}

impl NameTable {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Index> {
        // Nothing panics while the lock is held, so a poisoned lock still
        // guards a consistent index.
        self.index.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Index> {
        self.index.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn name(&self, index: u32) -> Name {
        Name {
            index,
            #[cfg(debug_assertions)]
            table: self.tag,
        }
    }

    fn path(&self, index: u32) -> PathId {
        PathId {
            index,
            #[cfg(debug_assertions)]
            table: self.tag,
        }
    }

    #[cfg(debug_assertions)]
    fn check_name(&self, name: Name) {
        assert_eq!(
            name.table, self.tag,
            "a Name resolved against another name table"
        );
    }

    #[cfg(not(debug_assertions))]
    fn check_name(&self, _: Name) {}

    #[cfg(debug_assertions)]
    fn check_path(&self, path: PathId) {
        assert_eq!(
            path.table, self.tag,
            "a PathId resolved against another name table"
        );
    }

    #[cfg(not(debug_assertions))]
    fn check_path(&self, _: PathId) {}

    /// The name whose text is `text`, which is already a logical name:
    /// metadata names, and `id_text`'s output. Interns it if it is new.
    pub(crate) fn logical(&self, text: &str) -> Name {
        if let Some(&index) = self.read().names.get(text) {
            return self.name(index);
        }
        let mut index = self.write();
        // Another thread may have interned it between the two locks.
        if let Some(&existing) = index.names.get(text) {
            return self.name(existing);
        }
        let new = index.name_count;
        index.name_count = new.checked_add(1).expect("fewer than 2^32 names");
        self.texts.set(new, text.into());
        index.names.insert(text.into(), new);
        self.name(new)
    }

    /// The name whose text is `text`, if one was ever interned. Interns
    /// nothing.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn lookup(&self, text: &str) -> Option<Name> {
        self.read().names.get(text).map(|&index| self.name(index))
    }

    /// `name`'s text.
    pub(crate) fn text(&self, name: Name) -> &str {
        self.check_name(name);
        self.texts
            .get(name.index)
            .expect("a Name's text is written before the Name is issued")
    }

    /// The empty path.
    pub(crate) fn root(&self) -> PathId {
        self.path(0)
    }

    /// `parent` extended by `name`, if that path was ever interned. Interns
    /// nothing, so a `None` proves that no map keyed by [`PathId`]s from this
    /// table holds the path.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn child(&self, parent: PathId, name: Name) -> Option<PathId> {
        self.check_path(parent);
        self.check_name(name);
        self.read()
            .children
            .get(&(parent.index, name.index))
            .map(|&index| self.path(index))
    }

    /// `parent` extended by `name`, interned if it is new.
    pub(crate) fn intern_child(&self, parent: PathId, name: Name) -> PathId {
        if let Some(child) = self.child(parent, name) {
            return child;
        }
        let mut index = self.write();
        if let Some(&existing) = index.children.get(&(parent.index, name.index)) {
            return self.path(existing);
        }
        let new = index.path_count;
        index.path_count = new.checked_add(1).expect("fewer than 2^32 paths");
        self.paths.set(new - 1, PathNode { parent, last: name });
        index.children.insert((parent.index, name.index), new);
        self.path(new)
    }

    /// The path whose segments are `segments`, if it was ever interned.
    /// Interns nothing, and probes under one lock.
    pub(crate) fn lookup_path<S: AsRef<str>>(&self, segments: &[S]) -> Option<PathId> {
        let index = self.read();
        segments.iter().try_fold(self.root(), |parent, segment| {
            let name = *index.names.get(segment.as_ref())?;
            index
                .children
                .get(&(parent.index, name))
                .map(|&child| self.path(child))
        })
    }

    /// `path`'s parent and last name; `None` for the root.
    pub(crate) fn split(&self, path: PathId) -> Option<(PathId, Name)> {
        self.check_path(path);
        let slot = path.index.checked_sub(1)?;
        let node = self
            .paths
            .get(slot)
            .expect("a PathId's node is written before the PathId is issued");
        Some((node.parent, node.last))
    }

    /// `path`'s segments, outermost first.
    #[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
    pub(crate) fn segments(&self, path: PathId) -> Vec<&str> {
        let mut segments = Vec::new();
        let mut at = path;
        while let Some((parent, last)) = self.split(at) {
            segments.push(self.text(last));
            at = parent;
        }
        segments.reverse();
        segments
    }

    /// Every path the table holds, the root first, in the order they were
    /// interned.
    #[cfg(feature = "test-support")]
    pub(crate) fn paths(&self) -> impl Iterator<Item = PathId> + '_ {
        let count = self.read().path_count;
        (0..count).map(|index| self.path(index))
    }

    /// How many names the table holds: what the LSP bounds an env's table by.
    pub(crate) fn name_count(&self) -> usize {
        self.read().name_count as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A small alphabet, so generated texts and paths collide often.
    fn text() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![Just('a'), Just('b'), Just('+'), Just('`')],
            0..3,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    fn path() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(text(), 0..4)
    }

    fn intern_path(table: &NameTable, segments: &[String]) -> PathId {
        segments.iter().fold(table.root(), |parent, segment| {
            table.intern_child(parent, table.logical(segment))
        })
    }

    #[test]
    fn append_only_slots_cover_every_index_once() {
        let mut expected = 0usize;
        for chunk in 0..6 {
            for offset in 0..FIRST_CHUNK << chunk {
                let located = AppendOnly::<()>::locate(expected as u32);
                assert_eq!(located, (chunk, offset));
                expected += 1;
            }
        }
        assert_eq!(
            AppendOnly::<()>::locate(u32::MAX).0,
            CHUNKS - 1,
            "the last chunk holds the largest index"
        );
    }

    proptest! {
        /// `text(logical(s)) == s`, and one text has one name.
        #[test]
        fn a_name_resolves_to_its_text(texts in prop::collection::vec(text(), 0..20)) {
            let table = NameTable::default();
            let names: Vec<Name> = texts.iter().map(|t| table.logical(t)).collect();
            for (t, &n) in texts.iter().zip(&names) {
                prop_assert_eq!(table.text(n), t.as_str());
                prop_assert_eq!(table.lookup(t), Some(n));
            }
            for (i, a) in texts.iter().enumerate() {
                for (j, b) in texts.iter().enumerate() {
                    prop_assert_eq!(names[i] == names[j], a == b);
                }
            }
            let distinct: crate::hash::HashSet<&String> = texts.iter().collect();
            prop_assert_eq!(table.name_count(), distinct.len());
        }

        /// A path resolves to its segments, one path has one id, `child`
        /// agrees with `intern_child` and never inserts, and a path `child`
        /// misses was never interned.
        #[test]
        fn paths_name_segments_one_to_one(
            interned in prop::collection::vec(path(), 0..12),
            probed in prop::collection::vec(path(), 0..12),
        ) {
            let table = NameTable::default();
            let ids: Vec<PathId> = interned.iter().map(|p| intern_path(&table, p)).collect();
            for (p, &id) in interned.iter().zip(&ids) {
                prop_assert_eq!(table.segments(id), p.iter().map(String::as_str).collect::<Vec<_>>());
                prop_assert_eq!(table.lookup_path(p), Some(id));
            }
            for (i, a) in interned.iter().enumerate() {
                for (j, b) in interned.iter().enumerate() {
                    prop_assert_eq!(ids[i] == ids[j], a == b);
                }
            }
            let (names, paths) = {
                let index = table.read();
                (index.name_count, index.path_count)
            };
            for p in &probed {
                // The root and every prefix of an interned path are interned;
                // nothing else is.
                let interned_here = p.is_empty()
                    || interned
                        .iter()
                        .any(|q| q.len() >= p.len() && q[..p.len()] == p[..]);
                let found = table.lookup_path(p);
                prop_assert_eq!(found.is_some(), interned_here, "{:?}", p);
                // `child`, step by step, agrees with the whole-path probe.
                let stepwise = p.iter().try_fold(table.root(), |parent, segment| {
                    table.lookup(segment).and_then(|n| table.child(parent, n))
                });
                prop_assert_eq!(stepwise, found);
            }
            let index = table.read();
            prop_assert_eq!((index.name_count, index.path_count), (names, paths), "probes intern nothing");
            drop(index);
            for p in &probed {
                let id = intern_path(&table, p);
                prop_assert_eq!(table.lookup_path(p), Some(id));
            }
        }
    }

    /// Interning from many threads at once gives one id per text and per path.
    #[test]
    fn concurrent_interning_gives_one_id_per_text() {
        let table = NameTable::default();
        let texts: Vec<String> = (0..500).map(|i| format!("n{}", i % 97)).collect();
        let per_thread: Vec<Vec<(Name, PathId)>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|t| {
                    let table = &table;
                    let texts = &texts;
                    s.spawn(move || {
                        texts
                            .iter()
                            .cycle()
                            .skip(t * 13)
                            .take(texts.len())
                            .map(|text| {
                                let name = table.logical(text);
                                (name, table.intern_child(table.root(), name))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut by_text: crate::hash::HashMap<&str, (Name, PathId)> = Default::default();
        for ids in &per_thread {
            for &(name, path) in ids {
                let text = table.text(name);
                let first = *by_text.entry(text).or_insert((name, path));
                assert_eq!(first, (name, path), "{text}");
                assert_eq!(table.segments(path), vec![text]);
            }
        }
        assert_eq!(by_text.len(), 97);
        assert_eq!(table.name_count(), 97);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "different name tables")]
    fn comparing_names_from_two_tables_panics() {
        let a = NameTable::default();
        let b = NameTable::default();
        let _ = a.logical("x") == b.logical("x");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "another name table")]
    fn resolving_a_name_against_another_table_panics() {
        let a = NameTable::default();
        let b = NameTable::default();
        b.logical("x");
        let _ = b.text(a.logical("x"));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "different name tables")]
    fn comparing_paths_from_two_tables_panics() {
        let a = NameTable::default();
        let b = NameTable::default();
        let _ = a.root() == b.root();
    }
}
