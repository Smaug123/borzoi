//! [`TextPathSet`]: the string-keyed path trie [`PathSet`](super::PathSet)
//! replaced, kept as its reference under `interning-reference`
//! (`docs/sema-interning-plan.md`, D7): every query of a `PathSet` is
//! answered by this too, and the two must agree.

use crate::hash::HashMap;

/// A trie node's index in [`TextPathSet::nodes`]. The root is [`ROOT`].
type NodeId = usize;

const ROOT: NodeId = 0;

/// A multiset of paths (each a sequence of segments), with prefix queries
/// answered by walking a trie, and a cheap [`Self::mark`] / [`Self::rollback`]
/// pair for a scope that only adds to the set.
///
/// The trie's nodes sit in a flat `Vec`, so no walk of it (dropping or cloning
/// it included) recurses once per segment: a path is as long as the source
/// writes it, which nothing bounds. The log holds each insertion's node, not
/// its path, so the set's memory is linear in the segments inserted.
#[derive(Clone, Debug)]
pub(crate) struct TextPathSet {
    /// Every insertion, in order: what [`Self::rollback`] undoes, and what
    /// [`Self::iter`] walks.
    log: Vec<NodeId>,
    /// Each node's parent and segment; the root has none.
    parents: Vec<Option<(NodeId, String)>>,
    /// Each node's children by segment. A rollback leaves the nodes it empties
    /// in place, so there are at most as many as the segments ever inserted.
    nodes: Vec<HashMap<String, NodeId>>,
    /// How many times the path ending at each node occurs in `log`.
    members: Vec<usize>,
}

impl Default for TextPathSet {
    fn default() -> Self {
        TextPathSet {
            log: Vec::new(),
            parents: vec![None],
            nodes: vec![HashMap::default()],
            members: vec![0],
        }
    }
}

/// The state of a [`TextPathSet`] that a later [`TextPathSet::rollback`] returns it
/// to: how many insertions it held.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub(crate) struct TextPathSetMark(usize);

impl TextPathSetMark {
    /// The mark of a set that held `len` insertions.
    pub(crate) fn at(len: usize) -> Self {
        TextPathSetMark(len)
    }
}

impl TextPathSet {
    pub(crate) fn insert(&mut self, path: Vec<String>) {
        let mut node = ROOT;
        for seg in &path {
            node = match self.nodes[node].get(seg) {
                Some(&child) => child,
                None => {
                    let child = self.nodes.len();
                    self.nodes.push(HashMap::default());
                    self.parents.push(Some((node, seg.clone())));
                    self.members.push(0);
                    self.nodes[node].insert(seg.clone(), child);
                    child
                }
            };
        }
        self.members[node] += 1;
        self.log.push(node);
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
    pub(crate) fn contains(&self, path: &[String]) -> bool {
        self.member_prefix_lens(path).any(|len| len == path.len())
    }

    /// The lengths `k`, ascending, for which `names[..k]` followed by `suffix`
    /// is a member: a question about every prefix of `names` that reads each
    /// of its segments once, and from each prefix the trie holds, reads
    /// `suffix` only as far as the trie holds it. Nothing is built: no prefix
    /// of `names` is copied or joined to `suffix`.
    pub(crate) fn suffix_member_prefix_lens<'a, S: AsRef<str>>(
        &'a self,
        names: &'a [String],
        suffix: &'a [S],
    ) -> impl Iterator<Item = usize> + 'a {
        self.prefix_nodes(names)
            .enumerate()
            .filter(move |&(_, node)| {
                suffix
                    .iter()
                    .try_fold(node, |node, seg| {
                        self.nodes[node].get(seg.as_ref()).copied()
                    })
                    .is_some_and(|end| self.members[end] > 0)
            })
            .map(|(len, _)| len)
    }

    /// Whether some member is a prefix of `names` (`names` itself and the empty
    /// path included).
    pub(crate) fn has_prefix_of(&self, names: &[String]) -> bool {
        self.member_prefix_lens(names).next().is_some()
    }

    /// Whether some non-empty member is a prefix of `names` (`names` itself
    /// included).
    pub(crate) fn has_nonempty_prefix_of(&self, names: &[String]) -> bool {
        self.member_prefix_lens(names).any(|len| len > 0)
    }

    /// Whether some non-empty member is a *proper* prefix of `names`: `names`
    /// descends strictly under it.
    pub(crate) fn has_strict_nonempty_prefix_of(&self, names: &[String]) -> bool {
        self.member_prefix_lens(names)
            .any(|len| len > 0 && len < names.len())
    }

    /// Every insertion in order, a path inserted twice appearing twice.
    pub(crate) fn iter(&self) -> impl Iterator<Item = Vec<String>> + '_ {
        self.log.iter().map(|&node| {
            let mut path = Vec::new();
            let mut at = node;
            while let Some((parent, seg)) = &self.parents[at] {
                path.push(seg.clone());
                at = *parent;
            }
            path.reverse();
            path
        })
    }

    /// Undo every insertion made since `mark` was taken. The set must only
    /// have been inserted into since then: a rollback past a replacement of
    /// the whole set would restore nothing it meant to.
    pub(crate) fn rollback(&mut self, mark: TextPathSetMark) {
        assert!(
            mark.0 <= self.log.len(),
            "a TextPathSet rolled back to a mark taken from a larger set"
        );
        for node in self.log.split_off(mark.0) {
            self.members[node] = self.members[node]
                .checked_sub(1)
                .expect("every logged path is counted");
        }
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
            let mut set = TextPathSet::default();
            let mut model: Vec<Vec<String>> = Vec::new();
            let mut marks: Vec<(TextPathSetMark, usize)> = Vec::new();
            for op in ops {
                match op {
                    Op::Insert(p) => {
                        set.insert(p.clone());
                        model.push(p);
                    }
                    Op::Mark => marks.push((TextPathSetMark::at(set.log.len()), model.len())),
                    Op::Rollback => {
                        if let Some((mark, len)) = marks.pop() {
                            set.rollback(mark);
                            model.truncate(len);
                        }
                    }
                }
                prop_assert_eq!(set.iter().collect::<Vec<_>>(), model.clone());
                for q in &queries {
                    let under = |p: &&Vec<String>| q.starts_with(p.as_slice());
                    prop_assert_eq!(set.contains(q), model.iter().any(|p| p == q));
                    for suffix in &queries {
                        prop_assert_eq!(
                            set.suffix_member_prefix_lens(q, suffix).collect::<Vec<_>>(),
                            (0..=q.len())
                                .filter(|&k| model.iter().any(|p| {
                                    p.len() == k + suffix.len()
                                        && p[..k] == q[..k]
                                        && p[k..] == suffix[..]
                                }))
                                .collect::<Vec<_>>()
                        );
                    }
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
