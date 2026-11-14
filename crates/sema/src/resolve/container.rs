//! The walk's current container path, with the env-wide [`PathId`] of each
//! prefix.

use std::ops::Deref;

use crate::hash::HashMap;
use crate::names::{NameTable, PathId};

/// Dotted path of the enclosing *container*, and the [`PathId`] of each of
/// its prefixes, interned in the env's [`NameTable`]. Read as a `[String]`;
/// changed only through its methods, which keep the ids in step with the
/// segments.
///
/// A map keyed by a container's [`PathId`] answers for an enclosing container
/// without hashing that container's path, so a lookup across every enclosing
/// container costs one probe per level rather than one path hash per level.
/// The walk's maps get their keys from [`Self::id`] and [`Self::prefix_id`],
/// so a path the walk has not entered holds nothing in any of them, whether or
/// not the table holds it for some other walk.
#[derive(Debug)]
pub(super) struct ContainerPath<'n> {
    names: &'n NameTable,
    segments: Vec<String>,
    /// `prefix_ids[k]` is `segments[..k]`'s id: one more entry than
    /// `segments`.
    prefix_ids: Vec<PathId>,
}

impl Deref for ContainerPath<'_> {
    type Target = [String];

    fn deref(&self) -> &[String] {
        &self.segments
    }
}

impl<'n> ContainerPath<'n> {
    /// The root container, interning into `names`.
    pub(super) fn new(names: &'n NameTable) -> Self {
        ContainerPath {
            names,
            segments: Vec::new(),
            prefix_ids: vec![names.root()],
        }
    }

    /// The current container's id.
    pub(super) fn id(&self) -> PathId {
        *self
            .prefix_ids
            .last()
            .expect("the root's id is always present")
    }

    /// The ids of every enclosing container, the root first: one more than
    /// there are segments.
    pub(super) fn prefix_ids(&self) -> &[PathId] {
        &self.prefix_ids
    }

    /// The id of the enclosing container `self[..k]`.
    pub(super) fn prefix_id(&self, k: usize) -> PathId {
        self.prefix_ids[k]
    }

    /// The id of `path`, if the table holds it. `None` proves that no map
    /// keyed by [`PathId`]s holds the path; `Some` does not prove the walk
    /// entered it.
    pub(super) fn id_of(&self, path: &[String]) -> Option<PathId> {
        self.names.lookup_path(path)
    }

    /// What `map` holds for the container `path`: nothing when the walk has
    /// not entered it.
    pub(super) fn get_at<'m, V>(
        &self,
        map: &'m HashMap<PathId, V>,
        path: &[String],
    ) -> Option<&'m V> {
        map.get(&self.id_of(path)?)
    }

    /// Enter the container `segment` under the current one.
    pub(super) fn push(&mut self, segment: String) {
        let id = self
            .names
            .intern_child(self.id(), self.names.logical(&segment));
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
    id: PathId,
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

    /// Every id names exactly one path, every prefix's id is the one that
    /// path's lookup finds, and the table spells it.
    fn check(path: &ContainerPath, names: &NameTable, seen: &mut HashMap<PathId, Vec<String>>) {
        for k in 0..=path.len() {
            let prefix = path[..k].to_vec();
            let id = path.prefix_id(k);
            assert_eq!(path.id_of(&prefix), Some(id), "{prefix:?}");
            assert_eq!(names.segments(id), prefix, "{prefix:?}");
            let named = seen.entry(id).or_insert_with(|| prefix.clone());
            assert_eq!(*named, prefix, "one id for two paths");
        }
        assert_eq!(path.id(), path.prefix_id(path.len()));
    }

    fn walk(
        path: &mut ContainerPath,
        names: &NameTable,
        moves: &[Move],
        seen: &mut HashMap<PathId, Vec<String>>,
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
                    check(path, names, seen);
                    walk(path, names, body, seen);
                    assert_eq!(path.to_vec(), entered, "a body leaves what it enters");
                    path.leave_to(mark);
                    assert_eq!(path.to_vec(), before);
                }
            }
            check(path, names, seen);
        }
    }

    proptest! {
        /// Also run with paths another walk interned first, in the same
        /// table: ids differ, and nothing the walk can observe does.
        #[test]
        fn container_ids_name_paths_one_to_one(
            moves in moves(),
            earlier in prop::collection::vec(prop::collection::vec(segment(), 0..4), 0..6),
        ) {
            let names = NameTable::default();
            for p in &earlier {
                p.iter().fold(names.root(), |parent, s| names.intern_child(parent, names.logical(s)));
            }
            let mut path = ContainerPath::new(&names);
            let mut seen = HashMap::default();
            check(&path, &names, &mut seen);
            walk(&mut path, &names, &moves, &mut seen);
        }
    }

    #[test]
    fn an_uninterned_path_has_no_id() {
        let names = NameTable::default();
        let mut path = ContainerPath::new(&names);
        path.set(vec!["A".to_string(), "B".to_string()]);
        assert_eq!(path.id_of(&["A".to_string(), "C".to_string()]), None);
        assert_eq!(path.id_of(&["B".to_string()]), None);
        assert!(path.id_of(&["A".to_string()]).is_some());
    }

    #[test]
    #[should_panic(expected = "encloses")]
    fn leaving_to_a_container_that_does_not_enclose_panics() {
        let names = NameTable::default();
        let mut path = ContainerPath::new(&names);
        path.set(vec!["A".to_string()]);
        let mark = path.mark();
        path.set(vec!["B".to_string(), "C".to_string()]);
        path.leave_to(mark);
    }
}
