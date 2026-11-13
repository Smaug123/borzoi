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

    /// The ids of `segments`' prefixes, the empty one first, as far as the
    /// table holds them: `segments[..k]` is interned exactly when `k` is less
    /// than the result's length. Probes under one lock.
    pub(crate) fn lookup_path_prefixes<S: AsRef<str>>(&self, segments: &[S]) -> Vec<PathId> {
        let reader = self.reader();
        let mut ids = vec![self.root()];
        for segment in segments {
            let parent = *ids.last().expect("the root is always present");
            match reader
                .lookup(segment.as_ref())
                .and_then(|name| reader.child(parent, name))
            {
                Some(child) => ids.push(child),
                None => break,
            }
        }
        ids
    }

    /// The names of `segments`, and the ids of `base` extended by each prefix
    /// of `segments` (`base` itself first), each `None` where the table never
    /// interned it. Probes under one lock and interns nothing.
    pub(crate) fn lookup_extension<S: AsRef<str>>(
        &self,
        base: Option<PathId>,
        segments: &[S],
    ) -> Extension {
        let names = self.lookup_names(segments);
        let ids = self.extend_ids(base, &names);
        Extension { names, ids }
    }

    /// The names of `segments`, each `None` where the table never interned
    /// it. Probes under one lock and interns nothing.
    pub(crate) fn lookup_names<S: AsRef<str>>(&self, segments: &[S]) -> Vec<Option<Name>> {
        let reader = self.reader();
        segments
            .iter()
            .map(|segment| reader.lookup(segment.as_ref()))
            .collect()
    }

    /// The ids of `base` extended by each prefix of the path whose names are
    /// `names` (`base` itself first), each `None` where the table never
    /// interned it. Probes under one lock and interns nothing.
    pub(crate) fn extend_ids(
        &self,
        base: Option<PathId>,
        names: &[Option<Name>],
    ) -> Vec<Option<PathId>> {
        let reader = self.reader();
        let mut ids = Vec::with_capacity(names.len() + 1);
        ids.push(base);
        for name in names {
            let parent = *ids.last().expect("the base is always present");
            ids.push(
                parent
                    .zip(*name)
                    .and_then(|(parent, name)| reader.child(parent, name)),
            );
        }
        ids
    }

    /// The path whose segments are `segments`, interned if it is new.
    pub(crate) fn intern_path<S: AsRef<str>>(&self, segments: &[S]) -> PathId {
        segments.iter().fold(self.root(), |parent, segment| {
            self.intern_child(parent, self.logical(segment.as_ref()))
        })
    }

    /// A view of the table that probes without taking its lock again, for a
    /// run of probes. It holds the lock for reading, so nothing may intern
    /// into the table while it lives.
    pub(crate) fn reader(&self) -> NameReader<'_> {
        NameReader {
            table: self,
            index: self.read(),
        }
    }

    /// This table's identity, for a structure keyed by its ids to check, in
    /// a debug build, that it is probed with ids of the same table.
    #[cfg(debug_assertions)]
    pub(crate) fn tag(&self) -> TableTag {
        TableTag(self.tag)
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

    /// How many names the table holds.
    #[cfg(test)]
    pub(crate) fn name_count(&self) -> usize {
        self.read().name_count as usize
    }

    /// How many entries the table holds, names and paths together: what the
    /// LSP bounds an env's table by. Paths count apart from names, since
    /// rearranging names a table already holds interns new paths and no new
    /// name.
    pub(crate) fn entry_count(&self) -> usize {
        let index = self.read();
        index.name_count as usize + index.path_count as usize
    }
}

/// A dotted path's segments, each with its name in the table, looked up once
/// for every reading a tiered walk tries the path under.
#[derive(Debug)]
pub(crate) struct Segments {
    pub(crate) texts: Vec<String>,
    pub(crate) names: Vec<Option<Name>>,
}

impl Segments {
    /// `texts`, looked up in `names`. Interns nothing.
    pub(crate) fn new(names: &NameTable, texts: Vec<String>) -> Self {
        Segments {
            names: names.lookup_names(&texts),
            texts,
        }
    }
}

/// [`NameTable::lookup_extension`]'s answer: a base path extended by some
/// segments.
#[derive(Debug)]
pub(crate) struct Extension {
    /// Each segment's name.
    pub(crate) names: Vec<Option<Name>>,
    /// The base extended by the first `i` segments, at `i`: one more than
    /// there are segments.
    pub(crate) ids: Vec<Option<PathId>>,
}

/// A table's identity ([`NameTable::tag`]).
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TableTag(u32);

/// [`NameTable`]'s probes under one read lock ([`NameTable::reader`]).
pub(crate) struct NameReader<'t> {
    table: &'t NameTable,
    index: std::sync::RwLockReadGuard<'t, Index>,
}

impl NameReader<'_> {
    /// [`NameTable::lookup`].
    pub(crate) fn lookup(&self, text: &str) -> Option<Name> {
        self.index
            .names
            .get(text)
            .map(|&index| self.table.name(index))
    }

    /// [`NameTable::child`].
    pub(crate) fn child(&self, parent: PathId, name: Name) -> Option<PathId> {
        self.table.check_path(parent);
        self.table.check_name(name);
        self.index
            .children
            .get(&(parent.index, name.index))
            .map(|&index| self.table.path(index))
    }
}

/// A path as a query spells it: its id in the table, `None` when the table
/// never interned it, beside its segments.
///
/// The id answers the query. The segments are what the query's caller holds
/// independently of the table, and under `interning-reference` a debug build
/// asserts on every query that the two spell the same path and that the
/// string-keyed reference answers alike, so a key built against the wrong
/// parent or from the wrong text fails where it is used.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PathKey<'a> {
    pub(crate) id: Option<PathId>,
    pub(crate) text: &'a [String],
}

impl<'a> PathKey<'a> {
    /// The key of `text`, looked up in `names`. Interns nothing.
    pub(crate) fn of(names: &NameTable, text: &'a [String]) -> Self {
        PathKey {
            id: names.lookup_path(text),
            text,
        }
    }

    /// The key of `text`, whose id the caller already holds.
    pub(crate) fn new(id: Option<PathId>, text: &'a [String]) -> Self {
        PathKey { id, text }
    }

    /// Assert, under `interning-reference` in a debug build, that the id
    /// spells the segments.
    pub(crate) fn check(&self, names: &NameTable) {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            self.id,
            names.lookup_path(self.text),
            "a PathKey's id does not spell {:?}",
            self.text
        );
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = names;
    }
}

/// A name as a query spells it: its id in the table, `None` when the table
/// never interned it, beside its text. Checked like a [`PathKey`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct NameKey<'a> {
    pub(crate) id: Option<Name>,
    pub(crate) text: &'a str,
}

impl<'a> NameKey<'a> {
    /// The key of `text`, looked up in `names`. Interns nothing.
    pub(crate) fn of(names: &NameTable, text: &'a str) -> Self {
        NameKey {
            id: names.lookup(text),
            text,
        }
    }

    /// The key of `text`, whose id the caller already holds.
    pub(crate) fn new(id: Option<Name>, text: &'a str) -> Self {
        NameKey { id, text }
    }

    /// Assert, under `interning-reference` in a debug build, that the id
    /// spells the text.
    pub(crate) fn check(&self, names: &NameTable) {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            self.id,
            names.lookup(self.text),
            "a NameKey's id does not spell {:?}",
            self.text
        );
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = names;
    }
}

/// `path`'s id in `to`, given its id in `from`: the same segments, interned
/// in `to`. What re-keying an index onto another table maps each key by.
pub(crate) fn rekey_path(from: &NameTable, to: &NameTable, path: PathId) -> PathId {
    to.intern_path(&from.segments(path))
}

/// `name`'s id in `to`, given its id in `from`.
pub(crate) fn rekey_name(from: &NameTable, to: &NameTable, name: Name) -> Name {
    to.logical(from.text(name))
}

/// A set of interned paths. Under `interning-reference` in a debug build it
/// keeps the string-keyed set it replaced beside it and asserts on every
/// query that the two agree; in every debug build it asserts that it is only
/// ever used with one table's ids.
#[derive(Clone, Debug, Default)]
pub(crate) struct PathIdSet {
    ids: crate::hash::HashSet<PathId>,
    #[cfg(debug_assertions)]
    table: Option<TableTag>,
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    by_text: crate::hash::HashSet<Vec<String>>,
}

impl PathIdSet {
    #[cfg(debug_assertions)]
    fn check_table(&self, names: &NameTable) {
        if let Some(table) = self.table {
            assert_eq!(
                table,
                names.tag(),
                "a PathIdSet probed with another table's ids"
            );
        }
    }

    #[cfg(not(debug_assertions))]
    fn check_table(&self, _: &NameTable) {}

    /// Add `path`, interning it.
    pub(crate) fn insert(&mut self, names: &NameTable, path: &[String]) {
        self.check_table(names);
        #[cfg(debug_assertions)]
        {
            self.table = Some(names.tag());
        }
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        self.by_text.insert(path.to_vec());
        self.ids.insert(names.intern_path(path));
    }

    /// Whether `path` is a member.
    pub(crate) fn contains(&self, names: &NameTable, path: PathKey<'_>) -> bool {
        self.check_table(names);
        path.check(names);
        let found = path.id.is_some_and(|id| self.ids.contains(&id));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.by_text.contains(path.text),
            "an interned path set disagrees with its reference at {:?}",
            path.text
        );
        found
    }

    /// Whether `path` is a member, looked up only when the set has one.
    pub(crate) fn contains_path(&self, names: &NameTable, path: &[String]) -> bool {
        !self.is_empty() && self.contains(names, PathKey::of(names, path))
    }

    /// Whether the path whose id is `id` is a member, `false` when the table
    /// never interned it. Unlike [`Self::contains`], the caller holds no
    /// spelling to check against, so the reference is the caller's to assert.
    pub(crate) fn contains_id(&self, names: &NameTable, id: Option<PathId>) -> bool {
        self.check_table(names);
        id.is_some_and(|id| self.ids.contains(&id))
    }

    /// Whether the set has no member.
    pub(crate) fn is_empty(&self) -> bool {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(self.ids.is_empty(), self.by_text.is_empty());
        self.ids.is_empty()
    }

    /// The same set, keyed by `to`'s ids.
    pub(crate) fn rekey(&self, from: &NameTable, to: &NameTable) -> Self {
        let mut ids = crate::hash::HashSet::default();
        for &id in &self.ids {
            ids.insert(rekey_path(from, to, id));
        }
        PathIdSet {
            ids,
            #[cfg(debug_assertions)]
            table: self.table.map(|_| to.tag()),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            by_text: self.by_text.clone(),
        }
    }
}

/// A map keyed by interned paths, which a lookup by a path's segments or by
/// its id reaches. Under `interning-reference` in a debug build it keeps the
/// string-keyed map it replaced beside it and asserts on every query that the
/// two agree; in every debug build it asserts that it is only ever used with
/// one table's ids.
#[derive(Clone, Debug)]
pub(crate) struct PathMap<V> {
    by_id: HashMap<PathId, V>,
    #[cfg(debug_assertions)]
    table: Option<TableTag>,
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    by_text: HashMap<Vec<String>, V>,
}

impl<V> Default for PathMap<V> {
    fn default() -> Self {
        PathMap {
            by_id: HashMap::default(),
            #[cfg(debug_assertions)]
            table: None,
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            by_text: HashMap::default(),
        }
    }
}

impl<V: Clone + PartialEq + std::fmt::Debug> PathMap<V> {
    #[cfg(debug_assertions)]
    fn check_table(&self, names: &NameTable) {
        if let Some(table) = self.table {
            assert_eq!(
                table,
                names.tag(),
                "a PathMap probed with another table's ids"
            );
        }
    }

    #[cfg(not(debug_assertions))]
    fn check_table(&self, _: &NameTable) {}

    /// Map `path`, interning it, to `value`; the previous value goes.
    pub(crate) fn insert(&mut self, names: &NameTable, path: &[String], value: V) {
        self.check_table(names);
        #[cfg(debug_assertions)]
        {
            self.table = Some(names.tag());
        }
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        self.by_text.insert(path.to_vec(), value.clone());
        self.by_id.insert(names.intern_path(path), value);
    }

    /// Apply `update` to the value at `path`, interning it, starting from the
    /// default.
    pub(crate) fn update(&mut self, names: &NameTable, path: &[String], update: impl Fn(&mut V))
    where
        V: Default,
    {
        self.check_table(names);
        #[cfg(debug_assertions)]
        {
            self.table = Some(names.tag());
        }
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        update(self.by_text.entry(path.to_vec()).or_default());
        update(self.by_id.entry(names.intern_path(path)).or_default());
    }

    /// The value at `path`, looked up only when the map has an entry.
    pub(crate) fn get(&self, names: &NameTable, path: &[String]) -> Option<&V> {
        if self.by_id.is_empty() {
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            assert!(self.by_text.is_empty(), "a PathMap lost an entry");
            return None;
        }
        self.get_id(names, names.lookup_path(path), || path.to_vec())
    }

    /// The value at `path`, by a key the caller already holds.
    pub(crate) fn get_key(&self, names: &NameTable, path: PathKey<'_>) -> Option<&V> {
        path.check(names);
        self.get_id(names, path.id, || path.text.to_vec())
    }

    /// The value at the path whose id is `id`, `None` when the table never
    /// interned it; `text` spells the path, for the reference.
    pub(crate) fn get_id(
        &self,
        names: &NameTable,
        id: Option<PathId>,
        text: impl FnOnce() -> Vec<String>,
    ) -> Option<&V> {
        self.check_table(names);
        let found = id.and_then(|id| self.by_id.get(&id));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        {
            let text = text();
            assert_eq!(
                found,
                self.by_text.get(&text),
                "an interned path map disagrees with its reference at {text:?}"
            );
        }
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        drop(text);
        found
    }

    /// The same map, keyed by `to`'s ids.
    pub(crate) fn rekey(&self, from: &NameTable, to: &NameTable) -> Self {
        let mut by_id = HashMap::default();
        for (&id, v) in &self.by_id {
            by_id.insert(rekey_path(from, to, id), v.clone());
        }
        PathMap {
            by_id,
            #[cfg(debug_assertions)]
            table: self.table.map(|_| to.tag()),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            by_text: self.by_text.clone(),
        }
    }

    /// Every entry, as its path's id and value, in no particular order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (PathId, &V)> {
        self.by_id.iter().map(|(&id, v)| (id, v))
    }

    /// Every entry, as its path's id and value, held to the reference: each
    /// is spelled as the reference spells one, and they are as many. Costs a
    /// spelling per entry under the reference, so only a scan pays it.
    pub(crate) fn iter_checked<'m>(
        &'m self,
        names: &'m NameTable,
    ) -> impl Iterator<Item = (PathId, &'m V)> + 'm {
        self.check_table(names);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        {
            assert_eq!(
                self.by_id.len(),
                self.by_text.len(),
                "a PathMap lost an entry"
            );
            for (&id, v) in &self.by_id {
                let text: Vec<String> =
                    names.segments(id).into_iter().map(str::to_string).collect();
                assert_eq!(self.by_text.get(&text), Some(v), "{text:?}");
            }
        }
        self.iter()
    }
}

/// A map keyed by a container's interned path and a name in it. Under
/// `interning-reference` in a debug build it keeps the map it replaced,
/// keyed by the container's path and the name's text, beside it and asserts
/// on every query that the two agree.
#[derive(Clone, Debug)]
pub(crate) struct ChildMap<V> {
    by_id: HashMap<(PathId, Name), V>,
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    by_text: HashMap<PathId, HashMap<String, V>>,
}

impl<V> Default for ChildMap<V> {
    fn default() -> Self {
        ChildMap {
            by_id: HashMap::default(),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            by_text: HashMap::default(),
        }
    }
}

impl<V: Default + PartialEq + std::fmt::Debug> ChildMap<V> {
    /// Apply `update` to the value at `name` in `container`, starting from
    /// the default.
    pub(crate) fn update(
        &mut self,
        names: &NameTable,
        container: PathId,
        name: &str,
        update: impl Fn(&mut V),
    ) {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        update(
            self.by_text
                .entry(container)
                .or_default()
                .entry(name.to_string())
                .or_default(),
        );
        update(
            self.by_id
                .entry((container, names.logical(name)))
                .or_default(),
        );
    }

    /// The value at `name` in `container`. `name` is `None` when the table
    /// never interned it, and its text is `name_text`.
    pub(crate) fn get(&self, container: PathId, name: Option<Name>, name_text: &str) -> Option<&V> {
        let found = name.and_then(|name| self.by_id.get(&(container, name)));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.by_text
                .get(&container)
                .and_then(|by_name| by_name.get(name_text)),
            "an interned child map disagrees with its reference at {name_text:?}"
        );
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = name_text;
        found
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

    proptest! {
        /// `extend_ids` from an interned base agrees, prefix by prefix, with a
        /// whole-path lookup of the base's segments followed by that prefix,
        /// and interns nothing.
        #[test]
        fn extensions_agree_with_whole_path_lookups(
            interned in prop::collection::vec(path(), 0..12),
            base in path(),
            tail in path(),
        ) {
            let table = NameTable::default();
            for p in &interned {
                intern_path(&table, p);
            }
            let base_id = intern_path(&table, &base);
            let before = table.entry_count();
            let ext = table.lookup_extension(Some(base_id), &tail);
            prop_assert_eq!(ext.ids.len(), tail.len() + 1);
            for (k, id) in ext.ids.iter().enumerate() {
                let whole: Vec<String> = base.iter().chain(&tail[..k]).cloned().collect();
                prop_assert_eq!(*id, table.lookup_path(&whole), "{:?}", whole);
            }
            for (segment, name) in tail.iter().zip(&ext.names) {
                prop_assert_eq!(*name, table.lookup(segment));
            }
            prop_assert_eq!(table.entry_count(), before, "lookups intern nothing");
        }

        /// A `PathIdSet` answers as the string set it replaced, whether
        /// asked by key or by path, and re-keyed onto another table.
        #[test]
        fn path_id_sets_agree_with_string_sets(
            members in prop::collection::vec(path(), 0..12),
            probes in prop::collection::vec(path(), 0..12),
        ) {
            let table = NameTable::default();
            let mut set = PathIdSet::default();
            for p in &members {
                set.insert(&table, p);
            }
            let other = NameTable::default();
            other.logical("decoy");
            let rekeyed = set.rekey(&table, &other);
            for p in members.iter().chain(&probes) {
                let expected = members.contains(p);
                prop_assert_eq!(set.contains(&table, PathKey::of(&table, p)), expected);
                prop_assert_eq!(set.contains_path(&table, p), expected);
                prop_assert_eq!(rekeyed.contains_path(&other, p), expected);
            }
        }
    }

    /// The entry count grows with paths that reuse names the table holds.
    #[test]
    fn paths_over_known_names_count_as_entries() {
        let table = NameTable::default();
        let (a, b) = (table.logical("A"), table.logical("B"));
        let before = table.entry_count();
        let mut path = table.root();
        for i in 0..8 {
            path = table.intern_child(path, if i % 2 == 0 { a } else { b });
        }
        assert_eq!(table.name_count(), 2);
        assert_eq!(table.entry_count(), before + 8);
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
