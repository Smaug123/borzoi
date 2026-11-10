//! [`PathSet`]: a set of qualified paths that answers "is any member a prefix
//! of this path?" in time linear in the path's length, not the set's size.
//!
//! The shadow sets it backs gain an entry for each module, type, exception and
//! augmentation head a file declares, and are asked about every name the walk
//! resolves. Scanning them per question costs time quadratic in a file's
//! declarations (`walk_cost_scaling`'s `records`); hashing each prefix of the
//! question afresh costs time quadratic in the path's length (its
//! `type_long_ident`). A trie reads each segment of the question once.

use std::collections::HashMap;

/// A trie node's index in [`PathSet::nodes`]. The root is [`ROOT`].
type NodeId = usize;

const ROOT: NodeId = 0;

/// A multiset of paths (each a sequence of segments), with prefix queries
/// answered by walking a trie, and a cheap [`Self::mark`] / [`Self::rollback`]
/// pair for a scope that only adds to the set.
///
/// The trie's nodes sit in a flat `Vec`, so no walk of it (dropping or cloning
/// it included) recurses once per segment: a path is as long as the source
/// writes it, which nothing bounds.
#[derive(Clone, Debug)]
pub(super) struct PathSet {
    /// Every insertion, in order: what [`Self::rollback`] undoes, and what
    /// [`Self::iter`] walks.
    log: Vec<Vec<String>>,
    /// Each node's children by segment. A rollback leaves the nodes it empties
    /// in place, so there are at most as many as the segments ever inserted.
    nodes: Vec<HashMap<String, NodeId>>,
    /// How many times the path ending at each node occurs in `log`.
    members: Vec<usize>,
}

impl Default for PathSet {
    fn default() -> Self {
        PathSet {
            log: Vec::new(),
            nodes: vec![HashMap::new()],
            members: vec![0],
        }
    }
}

/// The state of a [`PathSet`] that a later [`PathSet::rollback`] returns it
/// to: how many insertions it held.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub(super) struct PathSetMark(usize);

impl PathSet {
    pub(super) fn insert(&mut self, path: Vec<String>) {
        let mut node = ROOT;
        for seg in &path {
            node = match self.nodes[node].get(seg) {
                Some(&child) => child,
                None => {
                    let child = self.nodes.len();
                    self.nodes.push(HashMap::new());
                    self.members.push(0);
                    self.nodes[node].insert(seg.clone(), child);
                    child
                }
            };
        }
        self.members[node] += 1;
        self.log.push(path);
    }

    /// The nodes along `names`, one per prefix (the empty one first), as far
    /// as the trie holds them.
    fn prefix_nodes<'a>(&'a self, names: &'a [String]) -> impl Iterator<Item = NodeId> + 'a {
        let mut segs = names.iter();
        std::iter::successors(Some(ROOT), move |&node| {
            segs.next()
                .and_then(|seg| self.nodes[node].get(seg).copied())
        })
    }

    /// The members that are prefixes of `names`, by length.
    fn member_prefix_lens<'a>(&'a self, names: &'a [String]) -> impl Iterator<Item = usize> + 'a {
        self.prefix_nodes(names)
            .enumerate()
            .filter(|&(_, node)| self.members[node] > 0)
            .map(|(len, _)| len)
    }

    /// Whether `path` itself is a member.
    pub(super) fn contains(&self, path: &[String]) -> bool {
        self.member_prefix_lens(path).any(|len| len == path.len())
    }

    /// Whether some member is a prefix of `names` (`names` itself and the empty
    /// path included).
    pub(super) fn has_prefix_of(&self, names: &[String]) -> bool {
        self.member_prefix_lens(names).next().is_some()
    }

    /// Whether some non-empty member is a prefix of `names` (`names` itself
    /// included).
    pub(super) fn has_nonempty_prefix_of(&self, names: &[String]) -> bool {
        self.member_prefix_lens(names).any(|len| len > 0)
    }

    /// Whether some non-empty member is a *proper* prefix of `names`: `names`
    /// descends strictly under it.
    pub(super) fn has_strict_nonempty_prefix_of(&self, names: &[String]) -> bool {
        self.member_prefix_lens(names)
            .any(|len| len > 0 && len < names.len())
    }

    /// Every insertion in order, a path inserted twice appearing twice.
    pub(super) fn iter(&self) -> std::slice::Iter<'_, Vec<String>> {
        self.log.iter()
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
        for path in self.log.split_off(mark.0) {
            let node = self
                .prefix_nodes(&path)
                .nth(path.len())
                .expect("every logged path is in the trie");
            self.members[node] = self.members[node]
                .checked_sub(1)
                .expect("every logged path is counted");
        }
    }
}

impl FromIterator<Vec<String>> for PathSet {
    fn from_iter<I: IntoIterator<Item = Vec<String>>>(iter: I) -> Self {
        let mut set = PathSet::default();
        for path in iter {
            set.insert(path);
        }
        set
    }
}

impl<'a> IntoIterator for &'a PathSet {
    type Item = &'a Vec<String>;
    type IntoIter = std::slice::Iter<'a, Vec<String>>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
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
        /// interleaving of inserts and nested mark/rollback pairs.
        #[test]
        fn queries_agree_with_a_scan(
            ops in prop::collection::vec(op(), 0..40),
            queries in prop::collection::vec(path(), 1..8),
        ) {
            let mut set = PathSet::default();
            let mut model: Vec<Vec<String>> = Vec::new();
            let mut marks: Vec<(PathSetMark, usize)> = Vec::new();
            for op in ops {
                match op {
                    Op::Insert(p) => {
                        set.insert(p.clone());
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
                prop_assert_eq!(set.iter().cloned().collect::<Vec<_>>(), model.clone());
                for q in &queries {
                    let under = |p: &&Vec<String>| q.starts_with(p.as_slice());
                    prop_assert_eq!(set.contains(q), model.iter().any(|p| p == q));
                    prop_assert_eq!(set.has_prefix_of(q), model.iter().any(|p| under(&p)));
                    prop_assert_eq!(
                        set.has_nonempty_prefix_of(q),
                        model.iter().filter(|p| !p.is_empty()).any(|p| under(&p))
                    );
                    prop_assert_eq!(
                        set.has_strict_nonempty_prefix_of(q),
                        model
                            .iter()
                            .filter(|p| !p.is_empty() && p.len() < q.len())
                            .any(|p| under(&p))
                    );
                }
            }
        }
    }
}
