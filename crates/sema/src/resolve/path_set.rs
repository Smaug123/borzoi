//! [`PathSet`]: a set of qualified paths, keyed by the env's interned
//! [`PathId`]s, that answers "is any member a prefix of this path?" in time
//! linear in the path's length, not the set's size.
//!
//! The shadow sets it backs gain an entry for each module, type, exception and
//! augmentation head a file declares, and are asked about every name the walk
//! resolves. Scanning them per question costs time quadratic in a file's
//! declarations (`walk_cost_scaling`'s `records`); hashing each prefix of the
//! question afresh costs time quadratic in the path's length (its
//! `type_long_ident`). The question arrives as the ids of its prefixes
//! ([`PathPrefixes`]), and each is one integer probe.
//!
//! The ids come from the env's name table, whose trie of names stands in for
//! the trie this set kept of its own: a member is interned when it is
//! inserted, so a prefix the table never interned is no member, and a
//! question about a path under it stops there.

#[cfg(any(test, all(feature = "interning-reference", debug_assertions)))]
mod text;

use crate::hash::HashMap;
use crate::names::{Name, NameKey, NameTable, PathId, PathKey};

/// A multiset of paths, each interned in the env's name table, with prefix
/// queries by id and a cheap [`Self::mark`] / [`Self::rollback`] pair for a
/// scope that only adds to the set.
///
/// Under `interning-reference` in a debug build it keeps the string-keyed
/// trie it replaced ([`text::TextPathSet`]) beside it and asserts on every
/// query that the two agree; in every debug build it asserts that it is
/// only ever used with one table's ids.
#[derive(Clone, Debug, Default)]
pub(super) struct PathSet {
    /// Every insertion, in order: what [`Self::rollback`] undoes, and what
    /// [`Self::iter`] walks.
    log: Vec<PathId>,
    /// How many times each member occurs in `log`; an entry a rollback
    /// empties stays at zero.
    members: HashMap<PathId, usize>,
    #[cfg(debug_assertions)]
    table: Option<crate::names::TableTag>,
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    reference: text::TextPathSet,
}

/// The state of a [`PathSet`] that a later [`PathSet::rollback`] returns it
/// to: how many insertions it held.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub(super) struct PathSetMark(usize);

/// A path asked about, as the ids of its prefixes: `ids[k]` is `text[..k]`'s
/// id, `None` where the table never interned it (and then at every longer
/// prefix too). One more id than segments.
#[derive(Clone, Debug)]
pub(super) struct PathPrefixes<'a> {
    pub(super) text: &'a [String],
    ids: Vec<Option<PathId>>,
}

impl<'a> PathPrefixes<'a> {
    /// The prefixes of `text`, looked up in `names`. Interns nothing.
    pub(super) fn of(names: &NameTable, text: &'a [String]) -> Self {
        PathPrefixes {
            text,
            ids: names.lookup_extension(Some(names.root()), text).ids,
        }
    }

    /// The prefixes of `text`, which is `base`'s segments followed by
    /// segments whose names are `tail`: `base`'s ancestors by id, when the
    /// caller holds it, then `base` extended by `tail`. Interns nothing.
    pub(super) fn under(
        names: &NameTable,
        base: PathKey<'_>,
        tail: &[Option<Name>],
        text: &'a [String],
    ) -> Self {
        debug_assert_eq!(base.text.len() + tail.len(), text.len());
        debug_assert_eq!(base.text, &text[..base.text.len()]);
        let mut ids = Vec::with_capacity(text.len() + 1);
        let reader = names.reader();
        match base.id {
            Some(id) => {
                ids.extend(
                    std::iter::successors(Some(id), |&at| names.split(at).map(|(p, _)| p))
                        .map(Some),
                );
                ids.reverse();
            }
            None => {
                let mut at = Some(names.root());
                ids.push(at);
                for segment in base.text {
                    at = at.and_then(|p| reader.child(p, reader.lookup(segment)?));
                    ids.push(at);
                }
            }
        }
        let mut at = base.id;
        for &name in tail {
            at = at.zip(name).and_then(|(p, n)| reader.child(p, n));
            ids.push(at);
        }
        drop(reader);
        let prefixes = PathPrefixes { text, ids };
        prefixes.check(names);
        prefixes
    }

    /// The prefixes of the container path whose prefix ids are `ids`
    /// (`ContainerPath::prefix_ids`), every one of them interned.
    pub(super) fn interned(ids: &[PathId], text: &'a [String]) -> Self {
        debug_assert_eq!(ids.len(), text.len() + 1);
        PathPrefixes {
            text,
            ids: ids.iter().copied().map(Some).collect(),
        }
    }

    /// `text[..k]`'s id.
    pub(super) fn id(&self, k: usize) -> Option<PathId> {
        self.ids[k]
    }

    /// Every prefix of this path extended by the segments whose names are
    /// `suffix` (spelled `suffix_text`), each by id, under one lock: the
    /// question a [`PathSet::suffix_member_prefix_lens`] asks of each set.
    pub(super) fn extended<'s, S: AsRef<str>>(
        &self,
        names: &NameTable,
        suffix: &[Option<Name>],
        suffix_text: &'s [S],
    ) -> Extended<'a, 's, S> {
        debug_assert_eq!(suffix.len(), suffix_text.len());
        let reader = names.reader();
        let ids = self
            .ids
            .iter()
            .map(|&base| {
                suffix
                    .iter()
                    .try_fold(base?, |parent, &name| reader.child(parent, name?))
            })
            .collect();
        Extended {
            base: self.text,
            suffix: suffix_text,
            ids,
        }
    }

    /// The whole path's key.
    pub(super) fn key(&self) -> PathKey<'a> {
        PathKey::new(
            *self.ids.last().expect("the empty prefix is always present"),
            self.text,
        )
    }

    /// Assert, under `interning-reference` in a debug build, that every id
    /// spells its prefix.
    fn check(&self, names: &NameTable) {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        for (k, &id) in self.ids.iter().enumerate() {
            PathKey::new(id, &self.text[..k]).check(names);
        }
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = names;
    }
}

/// [`PathPrefixes::extended`]: `base[..k]` followed by `suffix`, for every
/// `k`, by id (`None` where the table never interned it).
#[derive(Debug)]
pub(super) struct Extended<'a, 's, S> {
    base: &'a [String],
    suffix: &'s [S],
    ids: Vec<Option<PathId>>,
}

impl PathSet {
    #[cfg(debug_assertions)]
    fn check_table(&self, names: &NameTable) {
        if let Some(table) = self.table {
            assert_eq!(
                table,
                names.tag(),
                "a PathSet probed with another table's ids"
            );
        }
    }

    #[cfg(not(debug_assertions))]
    fn check_table(&self, _: &NameTable) {}

    fn member(&self, id: Option<PathId>) -> bool {
        id.and_then(|id| self.members.get(&id))
            .is_some_and(|&count| count > 0)
    }

    /// Whether nothing was inserted since the set was made (or every
    /// insertion was rolled back).
    pub(super) fn is_empty(&self) -> bool {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(self.log.is_empty(), self.reference.iter().next().is_none());
        self.log.is_empty()
    }

    /// Add `path`, interning it.
    pub(super) fn insert(&mut self, names: &NameTable, path: &[String]) {
        self.insert_id(names, names.intern_path(path), || path.to_vec());
    }

    /// Add the path whose id is `id`. `text` spells it, for the reference.
    pub(super) fn insert_id(
        &mut self,
        names: &NameTable,
        id: PathId,
        text: impl FnOnce() -> Vec<String>,
    ) {
        self.check_table(names);
        #[cfg(debug_assertions)]
        {
            self.table = Some(names.tag());
        }
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        self.reference.insert(text());
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        drop(text);
        *self.members.entry(id).or_default() += 1;
        self.log.push(id);
    }

    /// Whether `path` itself is a member.
    pub(super) fn contains(&self, names: &NameTable, path: PathKey<'_>) -> bool {
        self.check_table(names);
        path.check(names);
        let found = self.member(path.id);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.reference.contains(path.text),
            "an interned path set disagrees with its reference at {:?}",
            path.text
        );
        found
    }

    /// Whether `parent` extended by `name` is a member, by id: the path is
    /// spelled only for the reference.
    pub(super) fn contains_child(
        &self,
        names: &NameTable,
        parent: PathKey<'_>,
        name: NameKey<'_>,
    ) -> bool {
        self.check_table(names);
        parent.check(names);
        name.check(names);
        let found = !self.log.is_empty()
            && self.member(
                parent
                    .id
                    .zip(name.id)
                    .and_then(|(parent, name)| names.child(parent, name)),
            );
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        {
            let mut path = parent.text.to_vec();
            path.push(name.text.to_string());
            assert_eq!(
                found,
                self.reference.contains(&path),
                "an interned path set disagrees with its reference at {path:?}"
            );
        }
        found
    }

    /// Whether the path whose id is `id` is a member, `false` when the table
    /// never interned it. The caller holds no spelling to check against, so
    /// the reference is the caller's to assert.
    pub(super) fn contains_id(&self, names: &NameTable, id: Option<PathId>) -> bool {
        self.check_table(names);
        self.member(id)
    }

    /// [`Self::contains`] by the path's segments, looked up only when the
    /// set has a member.
    pub(super) fn contains_path(&self, names: &NameTable, path: &[String]) -> bool {
        if self.log.is_empty() {
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            assert!(!self.reference.contains(path));
            return false;
        }
        self.contains(names, PathKey::of(names, path))
    }

    /// Whether some `k` that `keep` accepts has `path[..k]` a member, walking
    /// `path`'s prefixes by id from the root without building anything; the
    /// walk stops where the table holds no longer prefix.
    fn any_member_prefix(
        &self,
        names: &NameTable,
        path: &[String],
        keep: impl Fn(usize) -> bool,
    ) -> bool {
        self.check_table(names);
        if self.log.is_empty() {
            return false;
        }
        let reader = names.reader();
        let mut at = names.root();
        if keep(0) && self.member(Some(at)) {
            return true;
        }
        for (k, segment) in path.iter().enumerate() {
            match reader
                .lookup(segment)
                .and_then(|name| reader.child(at, name))
            {
                Some(child) => at = child,
                None => return false,
            }
            if keep(k + 1) && self.member(Some(at)) {
                return true;
            }
        }
        false
    }

    /// [`Self::has_prefix_of`] by the path's segments, building nothing.
    pub(super) fn has_prefix_of_path(&self, names: &NameTable, path: &[String]) -> bool {
        let found = self.any_member_prefix(names, path, |_| true);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(found, self.reference.has_prefix_of(path), "{path:?}");
        found
    }

    /// Whether some non-empty member is a prefix of `path` (`path` itself
    /// included).
    pub(super) fn has_nonempty_prefix_of_path(&self, names: &NameTable, path: &[String]) -> bool {
        let found = self.any_member_prefix(names, path, |k| k > 0);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.reference.has_nonempty_prefix_of(path),
            "{path:?}"
        );
        found
    }

    /// Whether some non-empty member is a *proper* prefix of `path`: `path`
    /// descends strictly under it.
    pub(super) fn has_strict_nonempty_prefix_of_path(
        &self,
        names: &NameTable,
        path: &[String],
    ) -> bool {
        let len = path.len();
        let found = self.any_member_prefix(names, path, |k| k > 0 && k < len);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.reference.has_strict_nonempty_prefix_of(path),
            "{path:?}"
        );
        found
    }

    /// The lengths `k`, ascending, at which `path[..k]` is a member.
    fn member_prefix_lens<'q>(
        &'q self,
        names: &NameTable,
        path: &'q PathPrefixes<'_>,
    ) -> impl Iterator<Item = usize> + 'q {
        self.check_table(names);
        path.ids
            .iter()
            .enumerate()
            .filter(|&(_, &id)| self.member(id))
            .map(|(k, _)| k)
    }

    /// Whether some member is a prefix of `path` (`path` itself and the empty
    /// path included).
    pub(super) fn has_prefix_of(&self, names: &NameTable, path: &PathPrefixes<'_>) -> bool {
        let found = self.member_prefix_lens(names, path).next().is_some();
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.reference.has_prefix_of(path.text),
            "{:?}",
            path.text
        );
        found
    }

    /// The lengths `k`, ascending, for which `base[..k]` followed by the
    /// suffix is a member, of an [`Extended`] question. Nothing is built.
    pub(super) fn suffix_member_prefix_lens<S: AsRef<str>>(
        &self,
        names: &NameTable,
        question: &Extended<'_, '_, S>,
    ) -> Vec<usize> {
        self.check_table(names);
        let found: Vec<usize> = if self.log.is_empty() {
            Vec::new()
        } else {
            question
                .ids
                .iter()
                .enumerate()
                .filter(|&(_, &id)| self.member(id))
                .map(|(k, _)| k)
                .collect()
        };
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.reference
                .suffix_member_prefix_lens(question.base, question.suffix)
                .collect::<Vec<_>>(),
            "{:?} + {:?}",
            question.base,
            question
                .suffix
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>()
        );
        found
    }

    /// Every insertion in order, a path inserted twice appearing twice.
    pub(super) fn iter(&self) -> impl Iterator<Item = PathId> + '_ {
        self.log.iter().copied()
    }

    /// Every insertion in order, as segments, held to the reference. Costs a
    /// spelling per member, so only a scan pays it.
    #[cfg(test)]
    pub(super) fn iter_text<'s>(
        &'s self,
        names: &'s NameTable,
    ) -> impl Iterator<Item = Vec<String>> + 's {
        self.check_table(names);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            self.log
                .iter()
                .map(|&id| {
                    names
                        .segments(id)
                        .into_iter()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>(),
            self.reference.iter().collect::<Vec<_>>(),
            "a PathSet's insertions disagree with its reference"
        );
        self.log
            .iter()
            .map(move |&id| names.segments(id).into_iter().map(str::to_string).collect())
    }

    pub(super) fn mark(&self) -> PathSetMark {
        PathSetMark(self.log.len())
    }

    /// Undo every insertion made since `mark` was taken. The set must only
    /// have been inserted into since then: a rollback past a replacement of
    /// the whole set would restore nothing it meant to.
    pub(super) fn rollback(&mut self, mark: PathSetMark) {
        assert!(
            mark.0 <= self.log.len(),
            "a PathSet rolled back to a mark taken from a larger set"
        );
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        self.reference.rollback(self.reference_mark(mark));
        for id in self.log.split_off(mark.0) {
            let count = self
                .members
                .get_mut(&id)
                .expect("every logged path is counted");
            *count = count.checked_sub(1).expect("every logged path is counted");
        }
    }

    #[cfg(all(feature = "interning-reference", debug_assertions))]
    fn reference_mark(&self, mark: PathSetMark) -> text::TextPathSetMark {
        text::TextPathSetMark::at(mark.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A path over a two-segment alphabet, so prefixes collide often.
    fn path() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(prop::sample::select(vec!["A", "B"]), 0..4)
            .prop_map(|segs| segs.into_iter().map(String::from).collect())
    }

    #[derive(Clone, Debug)]
    enum Op {
        Insert(Vec<String>),
        Mark,
        Rollback,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => path().prop_map(Op::Insert),
            1 => Just(Op::Mark),
            1 => Just(Op::Rollback),
        ]
    }

    proptest! {
        /// Every query agrees with a scan of the insertions, under any
        /// interleaving of inserts and nested mark/rollback pairs, with the
        /// question's prefixes looked up from the root or built under an
        /// interned base.
        #[test]
        fn queries_agree_with_a_scan(
            ops in prop::collection::vec(op(), 0..40),
            queries in prop::collection::vec(path(), 1..8),
        ) {
            let names = NameTable::default();
            // A decoy, so the table holds paths no set does.
            names.intern_path(&["B".to_string(), "B".to_string(), "A".to_string()]);
            let mut set = PathSet::default();
            let mut model: Vec<Vec<String>> = Vec::new();
            let mut marks: Vec<(PathSetMark, usize)> = Vec::new();
            for op in ops {
                match op {
                    Op::Insert(p) => {
                        set.insert(&names, &p);
                        model.push(p);
                    }
                    Op::Mark => marks.push((set.mark(), model.len())),
                    Op::Rollback => {
                        if let Some((mark, len)) = marks.pop() {
                            set.rollback(mark);
                            model.truncate(len);
                        }
                    }
                }
                prop_assert_eq!(set.iter_text(&names).collect::<Vec<_>>(), model.clone());
                for q in &queries {
                    let under = |p: &&Vec<String>| q.starts_with(p.as_slice());
                    let split = q.len() / 2;
                    let base_id = names.intern_path(&q[..split]);
                    let tail = names.lookup_names(&q[split..]);
                    for prefixes in [
                        PathPrefixes::of(&names, q),
                        PathPrefixes::under(&names, PathKey::new(Some(base_id), &q[..split]), &tail, q),
                        PathPrefixes::under(&names, PathKey::of(&names, &q[..split]), &tail, q),
                    ] {
                        prop_assert_eq!(set.contains(&names, prefixes.key()), model.iter().any(|p| p == q));
                        prop_assert_eq!(set.has_prefix_of(&names, &prefixes), model.iter().any(|p| under(&p)));
                        prop_assert_eq!(
                            set.has_nonempty_prefix_of_path(&names, q),
                            model.iter().filter(|p| !p.is_empty()).any(|p| under(&p))
                        );
                        prop_assert_eq!(
                            set.has_strict_nonempty_prefix_of_path(&names, q),
                            model
                                .iter()
                                .filter(|p| !p.is_empty() && p.len() < q.len())
                                .any(|p| under(&p))
                        );
                        for suffix in &queries {
                            let suffix_names = names.lookup_names(suffix);
                            prop_assert_eq!(
                                set.suffix_member_prefix_lens(
                                    &names,
                                    &prefixes.extended(&names, &suffix_names, suffix),
                                ),
                                (0..=q.len())
                                    .filter(|&k| model.iter().any(|p| {
                                        p.len() == k + suffix.len()
                                            && p[..k] == q[..k]
                                            && p[k..] == suffix[..]
                                    }))
                                    .collect::<Vec<_>>()
                            );
                        }
                    }
                    prop_assert_eq!(set.contains_path(&names, q), model.iter().any(|p| p == q));
                    prop_assert_eq!(set.has_prefix_of_path(&names, q), model.iter().any(|p| under(&p)));
                }
            }
        }
    }
}
