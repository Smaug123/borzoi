//! The walk's current container path, with an interned id for each prefix.

use std::ops::Deref;

use crate::hash::HashMap;

/// A container path the walk has entered, interned by [`ContainerPath`]: a
/// map keyed by it answers for an enclosing container without hashing that
/// container's path, so a lookup across every enclosing container costs one
/// probe per level rather than one path hash per level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct ContainerId(u32);

impl ContainerId {
    /// The empty path: an anonymous module or `namespace global`.
    const ROOT: ContainerId = ContainerId(0);
}

/// Dotted path of the enclosing *container*, and the [`ContainerId`] of each
/// of its prefixes. Read as a `[String]`; changed only through its methods,
/// which keep the ids in step with the segments.
#[derive(Debug)]
pub(super) struct ContainerPath {
    segments: Vec<String>,
    /// `prefix_ids[k]` is `segments[..k]`'s id: one more entry than
    /// `segments`.
    prefix_ids: Vec<ContainerId>,
    /// Every path entered so far, by its parent's id and then its last
    /// segment.
    children: HashMap<ContainerId, HashMap<String, ContainerId>>,
    /// How many paths have been entered: the next path's id.
    entered: u32,
}

impl Default for ContainerPath {
    fn default() -> Self {
        ContainerPath {
            segments: Vec::new(),
            prefix_ids: vec![ContainerId::ROOT],
            children: HashMap::default(),
            entered: 1,
        }
    }
}

impl Deref for ContainerPath {
    type Target = [String];

    fn deref(&self) -> &[String] {
        &self.segments
    }
}

impl ContainerPath {
    /// The current container's id.
    pub(super) fn id(&self) -> ContainerId {
        *self
            .prefix_ids
            .last()
            .expect("the root's id is always present")
    }

    /// The id of the enclosing container `self[..k]`.
    pub(super) fn prefix_id(&self, k: usize) -> ContainerId {
        self.prefix_ids[k]
    }

    /// The id of `path`, if the walk has entered it; a container it has not
    /// entered holds nothing in any map keyed by [`ContainerId`].
    pub(super) fn id_of(&self, path: &[String]) -> Option<ContainerId> {
        path.iter().try_fold(ContainerId::ROOT, |parent, segment| {
            self.children.get(&parent)?.get(segment).copied()
        })
    }

    /// What `map` holds for the container `path`: nothing when the walk has
    /// not entered it.
    pub(super) fn get_at<'m, V>(
        &self,
        map: &'m HashMap<ContainerId, V>,
        path: &[String],
    ) -> Option<&'m V> {
        map.get(&self.id_of(path)?)
    }

    /// Enter the container `segment` under the current one.
    pub(super) fn push(&mut self, segment: String) {
        let parent = self.id();
        let siblings = self.children.entry(parent).or_default();
        let id = match siblings.get(&segment) {
            Some(&id) => id,
            None => {
                let id = ContainerId(self.entered);
                self.entered = self
                    .entered
                    .checked_add(1)
                    .expect("fewer than 2^32 containers");
                siblings.insert(segment.clone(), id);
                id
            }
        };
        self.segments.push(segment);
        self.prefix_ids.push(id);
    }

    /// Replace the whole path.
    pub(super) fn set(&mut self, path: Vec<String>) {
        self.truncate(0);
        for segment in path {
            self.push(segment);
        }
    }

    /// Where the walk is now, for [`Self::leave_to`] to return to.
    pub(super) fn mark(&self) -> ContainerMark {
        ContainerMark {
            len: self.segments.len(),
            id: self.id(),
        }
    }

    /// Return to `mark`, an enclosing container of the current one.
    pub(super) fn leave_to(&mut self, mark: ContainerMark) {
        self.truncate(mark.len);
        assert_eq!(
            self.id(),
            mark.id,
            "a container is left only for one that encloses it"
        );
    }

    fn truncate(&mut self, len: usize) {
        assert!(
            len <= self.segments.len(),
            "a container path only shrinks by truncation"
        );
        self.segments.truncate(len);
        self.prefix_ids.truncate(len + 1);
    }
}

/// A container the walk was in ([`ContainerPath::mark`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct ContainerMark {
    len: usize,
    id: ContainerId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// One move of the walk over containers.
    #[derive(Clone, Debug)]
    enum Move {
        /// A top-level block's header.
        Set(Vec<String>),
        /// A nested module, entered, its body walked, then left.
        Nested(Vec<String>, Vec<Move>),
    }

    fn segment() -> impl Strategy<Value = String> {
        prop_oneof![Just("A"), Just("B"), Just("C")].prop_map(str::to_string)
    }

    /// A file's blocks: each a header, then nested modules.
    fn moves() -> impl Strategy<Value = Vec<Move>> {
        let empty = Just(Vec::<Move>::new());
        let body = empty.prop_recursive(4, 32, 4, |inner| {
            prop::collection::vec(
                (prop::collection::vec(segment(), 1..3), inner)
                    .prop_map(|(segments, body)| Move::Nested(segments, body)),
                0..4,
            )
        });
        let block = (prop::collection::vec(segment(), 0..3), body).prop_map(|(header, body)| {
            let mut moves = vec![Move::Set(header)];
            moves.extend(body);
            moves
        });
        prop::collection::vec(block, 0..4).prop_map(|blocks| blocks.concat())
    }

    /// Every id names exactly one path, and every prefix's id is the one that
    /// path's lookup finds.
    fn check(path: &ContainerPath, seen: &mut HashMap<ContainerId, Vec<String>>) {
        for k in 0..=path.len() {
            let prefix = path[..k].to_vec();
            let id = path.prefix_id(k);
            assert_eq!(path.id_of(&prefix), Some(id), "{prefix:?}");
            let named = seen.entry(id).or_insert_with(|| prefix.clone());
            assert_eq!(*named, prefix, "one id for two paths");
        }
        assert_eq!(path.id(), path.prefix_id(path.len()));
    }

    fn walk(
        path: &mut ContainerPath,
        moves: &[Move],
        seen: &mut HashMap<ContainerId, Vec<String>>,
    ) {
        for m in moves {
            match m {
                Move::Set(segments) => {
                    path.set(segments.clone());
                    assert_eq!(&**path, segments.as_slice());
                }
                Move::Nested(segments, body) => {
                    let before = path.to_vec();
                    let mark = path.mark();
                    for segment in segments {
                        path.push(segment.clone());
                    }
                    let mut entered = before.clone();
                    entered.extend(segments.iter().cloned());
                    assert_eq!(path.to_vec(), entered);
                    check(path, seen);
                    walk(path, body, seen);
                    assert_eq!(path.to_vec(), entered, "a body leaves what it enters");
                    path.leave_to(mark);
                    assert_eq!(path.to_vec(), before);
                }
            }
            check(path, seen);
        }
    }

    proptest! {
        #[test]
        fn container_ids_name_paths_one_to_one(moves in moves()) {
            let mut path = ContainerPath::default();
            let mut seen = HashMap::default();
            check(&path, &mut seen);
            walk(&mut path, &moves, &mut seen);
        }
    }

    #[test]
    fn an_unentered_path_has_no_id() {
        let mut path = ContainerPath::default();
        path.set(vec!["A".to_string(), "B".to_string()]);
        assert_eq!(path.id_of(&["A".to_string(), "C".to_string()]), None);
        assert_eq!(path.id_of(&["B".to_string()]), None);
        assert!(path.id_of(&["A".to_string()]).is_some());
    }

    #[test]
    #[should_panic(expected = "encloses")]
    fn leaving_to_a_container_that_does_not_enclose_panics() {
        let mut path = ContainerPath::default();
        path.set(vec!["A".to_string()]);
        let mark = path.mark();
        path.set(vec!["B".to_string(), "C".to_string()]);
        path.leave_to(mark);
    }
}
