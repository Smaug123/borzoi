//! [`DeclPath`]: the path an export declaration names, as an interned
//! [`PathId`] (`docs/sema-interning-plan.md`, Stage 6a); and [`ExportName`],
//! the name an export publishes, as an interned [`Name`] (Stage 6c).

#[cfg(all(feature = "interning-reference", debug_assertions))]
use std::sync::Arc;

use super::container::ContainerPath;
use crate::names::{Name, NameTable, PathId, PathKey};

/// The qualified path an [`ExportDecl`](super::model::ExportDecl) names, held
/// by its id in the env's [`NameTable`], so a declaration and every index
/// entry derived from it costs one integer, whatever the path's length.
///
/// Under `interning-reference` in a debug build it also carries the spelling
/// the walk built it from, by the string operations the id replaced, and
/// every operation below asserts that the id's answer agrees with that
/// spelling's. The spelling is shared (an `Arc`), so a clone, such as each
/// index entry [`ProjectItems::extend_with`](super::model::ProjectItems::extend_with)
/// derives from a declaration, copies no segment.
#[derive(Clone, Debug)]
pub(super) struct DeclPath {
    id: PathId,
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    text: Arc<[String]>,
}

impl PartialEq for DeclPath {
    fn eq(&self, other: &Self) -> bool {
        let same = self.id == other.id;
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            same,
            self.text == other.text,
            "two declaration paths' ids disagree with their spellings, {:?} and {:?}",
            self.text,
            other.text
        );
        same
    }
}

impl Eq for DeclPath {}

impl DeclPath {
    /// `path`, interned.
    pub(super) fn new<S: AsRef<str>>(names: &NameTable, path: &[S]) -> Self {
        let decl = DeclPath {
            id: names.intern_path(path),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: path.iter().map(|s| s.as_ref().to_string()).collect(),
        };
        decl.check(names);
        decl
    }

    /// The path whose id is `id`, which `text` spells.
    pub(super) fn interned(names: &NameTable, id: PathId, text: &[String]) -> Self {
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = text;
        let decl = DeclPath {
            id,
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: text.into(),
        };
        decl.check(names);
        decl
    }

    /// `container` extended by `segments`, interned. `container_text` spells
    /// `container`; only the reference reads it.
    pub(super) fn under<S: AsRef<str>>(
        names: &NameTable,
        container: PathId,
        container_text: &[String],
        segments: &[S],
    ) -> Self {
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = container_text;
        let decl = DeclPath {
            id: names.intern_extension(container, segments),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: container_text
                .iter()
                .cloned()
                .chain(segments.iter().map(|s| s.as_ref().to_string()))
                .collect(),
        };
        decl.check(names);
        decl
    }

    /// This path extended by `segments`, interned.
    pub(super) fn extended<S: AsRef<str>>(&self, names: &NameTable, segments: &[S]) -> Self {
        let decl = DeclPath {
            id: names.intern_extension(self.id, segments),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: self
                .text
                .iter()
                .cloned()
                .chain(segments.iter().map(|s| s.as_ref().to_string()))
                .collect(),
        };
        decl.check(names);
        decl
    }

    /// The same path, as an id of `to`'s; `self` is `from`'s.
    #[cfg(feature = "test-support")]
    pub(super) fn rekey(&self, from: &NameTable, to: &NameTable) -> Self {
        DeclPath {
            id: crate::names::rekey_path(from, to, self.id),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: Arc::clone(&self.text),
        }
    }

    /// The path's id.
    pub(super) fn id(&self) -> PathId {
        self.id
    }

    /// The path without its last segment; `None` for the empty path.
    pub(super) fn parent(&self, names: &NameTable) -> Option<DeclPath> {
        let parent = names.split(self.id).map(|(parent, _)| DeclPath {
            id: parent,
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: self.text[..self.text.len().saturating_sub(1)].into(),
        });
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            parent.is_some(),
            !self.text.is_empty(),
            "a declaration path's parent at {:?}",
            self.text
        );
        if let Some(parent) = &parent {
            parent.check(names);
        }
        parent
    }

    /// The path's last segment; `None` for the empty path.
    pub(super) fn last<'n>(&self, names: &'n NameTable) -> Option<&'n str> {
        let last = names.split(self.id).map(|(_, last)| names.text(last));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            last,
            self.text.last().map(String::as_str),
            "a declaration path's last segment at {:?}",
            self.text
        );
        last
    }

    /// Whether the path's segments are exactly `path`.
    pub(super) fn spells(&self, names: &NameTable, path: &[String]) -> bool {
        let found = names.lookup_path(path) == Some(self.id);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            *self.text == *path,
            "a declaration path compared with {path:?} at {:?}",
            self.text
        );
        found
    }

    /// Whether `prefix` is a prefix of the path, the path itself and the
    /// empty prefix included.
    pub(super) fn starts_with(&self, names: &NameTable, prefix: &[String]) -> bool {
        let found = names
            .lookup_path(prefix)
            .is_some_and(|prefix| names.is_prefix(prefix, self.id));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.text.starts_with(prefix),
            "a declaration path's prefix {prefix:?} at {:?}",
            self.text
        );
        found
    }

    /// Whether any of the path's segments is `segment`.
    pub(super) fn has_segment(&self, names: &NameTable, segment: &str) -> bool {
        let found = names
            .lookup(segment)
            .is_some_and(|segment| names.names_inward(self.id).any(|name| name == segment));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.text.iter().any(|s| s == segment),
            "a declaration path's segment {segment:?} at {:?}",
            self.text
        );
        found
    }

    /// The path's segments.
    pub(super) fn spell(&self, names: &NameTable) -> Vec<String> {
        let spelled: Vec<String> = names
            .segments(self.id)
            .into_iter()
            .map(str::to_string)
            .collect();
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            *spelled, *self.text,
            "a declaration path's id spells another path"
        );
        spelled
    }

    /// Whether the path is exactly `path`.
    pub(super) fn is(&self, names: &NameTable, path: PathKey<'_>) -> bool {
        path.check(names);
        let found = path.id == Some(self.id);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            *self.text == *path.text,
            "a declaration path compared with {:?} at {:?}",
            path.text,
            self.text
        );
        found
    }

    /// Whether the path is `parent` extended by one name.
    pub(super) fn is_child_of(&self, names: &NameTable, parent: PathKey<'_>) -> bool {
        parent.check(names);
        let found = parent
            .id
            .is_some_and(|parent| names.split(self.id).is_some_and(|(up, _)| up == parent));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.text.len() == parent.text.len() + 1 && self.text.starts_with(parent.text),
            "a declaration path's parent {:?} at {:?}",
            parent.text,
            self.text
        );
        found
    }

    /// Whether `prefix` is a proper prefix of the path.
    pub(super) fn is_strictly_under(&self, names: &NameTable, prefix: PathKey<'_>) -> bool {
        prefix.check(names);
        let found = prefix
            .id
            .is_some_and(|prefix| prefix != self.id && names.is_prefix(prefix, self.id));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            self.text.len() > prefix.text.len() && self.text.starts_with(prefix.text),
            "a declaration path's proper prefix {:?} at {:?}",
            prefix.text,
            self.text
        );
        found
    }

    /// Whether a use in `site` may see an export at this path whose access
    /// root is the path's first `access_root_len` segments
    /// ([`accessible_from`](super::model::accessible_from)): the root, when
    /// there is one, must be a prefix of `site`.
    ///
    /// # Panics
    ///
    /// Panics when the root is longer than the path.
    pub(super) fn accessible_from(
        &self,
        names: &NameTable,
        access_root_len: Option<usize>,
        site: &ContainerPath<'_>,
    ) -> bool {
        let found = access_root_len.is_none_or(|k| {
            let depth = names.names_inward(self.id).count();
            assert!(
                k <= depth,
                "an access root of {k} segments on a path of {depth}"
            );
            let root = (k..depth).fold(self.id, |at, _| {
                names
                    .split(at)
                    .expect("a path has a parent above each of its segments")
                    .0
            });
            site.prefix_ids().get(k) == Some(&root)
        });
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            found,
            super::model::accessible_from(access_root_len, &self.text, site),
            "a declaration path's access root {access_root_len:?} at {:?}, seen from {:?}",
            self.text,
            &**site
        );
        found
    }

    /// The path's segments for the reference of an index keyed by its id:
    /// the walk's own spelling where the reference is kept, so an index that
    /// files the id under another path disagrees with its reference at its
    /// next query.
    pub(super) fn reference_text(&self, names: &NameTable) -> Vec<String> {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        {
            let _ = names;
            self.text.to_vec()
        }
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        self.spell(names)
    }

    /// Assert that the id spells the walk's spelling.
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    fn check(&self, names: &NameTable) {
        let spelled = names.segments(self.id);
        assert!(
            spelled
                .iter()
                .copied()
                .eq(self.text.iter().map(String::as_str)),
            "a declaration path's id spells {spelled:?}, not {:?}",
            self.text
        );
    }

    #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
    fn check(&self, _: &NameTable) {}
}

/// The name an [`ExportedItem`](super::model::ExportedItem) publishes, in the
/// logical domain (`idText`: backticks stripped, a bare operator compiled),
/// held by its id in the env's [`NameTable`].
///
/// Under `interning-reference` in a debug build it also carries the text the
/// walk published it as, and reading it asserts that the id spells that
/// text, so an export whose name was interned from another spelling, or
/// carried across tables without re-keying, fails where it is read.
#[derive(Clone, Debug)]
pub(super) struct ExportName {
    name: Name,
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    text: Arc<str>,
}

impl PartialEq for ExportName {
    fn eq(&self, other: &Self) -> bool {
        let same = self.name == other.name;
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            same,
            self.text == other.text,
            "two export names' ids disagree with their texts, {:?} and {:?}",
            self.text,
            other.text
        );
        same
    }
}

impl Eq for ExportName {}

impl ExportName {
    /// `name`, which the walk spelled `text`.
    pub(super) fn new(names: &NameTable, name: Name, text: &str) -> Self {
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = text;
        let export = ExportName {
            name,
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: text.into(),
        };
        export.check(names);
        export
    }

    /// The name's text.
    pub(super) fn text<'n>(&self, names: &'n NameTable) -> &'n str {
        let text = names.text(self.name);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(text, &*self.text, "an export name's id spells another name");
        text
    }

    /// The same name, as an id of `to`'s; `self` is `from`'s.
    #[cfg(feature = "test-support")]
    pub(super) fn rekey(&self, from: &NameTable, to: &NameTable) -> Self {
        let export = ExportName {
            name: crate::names::rekey_name(from, to, self.name),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: Arc::clone(&self.text),
        };
        export.check(to);
        export
    }

    /// Assert that the id spells the walk's text.
    fn check(&self, names: &NameTable) {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        let _ = self.text(names);
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = names;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn segment() -> impl Strategy<Value = String> {
        prop_oneof![Just("A"), Just("B"), Just("C"), Just("op_Addition")].prop_map(str::to_string)
    }

    fn path() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(segment(), 0..5)
    }

    proptest! {
        /// Every operation on a [`DeclPath`] answers as the same operation on
        /// its segments does, whatever else the table holds and however the
        /// path was built: whole, under a container, or extended.
        #[test]
        fn a_declaration_path_answers_as_its_segments_do(
            interned in prop::collection::vec(path(), 0..6),
            container in path(),
            tail in path(),
            probes in prop::collection::vec(path(), 0..6),
            segments in prop::collection::vec(segment(), 0..4),
            sites in prop::collection::vec(path(), 0..4),
        ) {
            let names = NameTable::default();
            for path in &interned {
                names.intern_path(path);
            }
            let text: Vec<String> = container.iter().chain(&tail).cloned().collect();
            let whole = DeclPath::new(&names, &text);
            let under = DeclPath::under(&names, names.intern_path(&container), &container, &tail);
            let extended = DeclPath::new(&names, &container).extended(&names, &tail);
            for decl in [&whole, &under, &extended] {
                prop_assert_eq!(decl.spell(&names), text.clone());
                prop_assert_eq!(decl.reference_text(&names), text.clone());
                prop_assert_eq!(decl.last(&names), text.last().map(String::as_str));
                prop_assert_eq!(
                    decl.parent(&names).map(|p| p.spell(&names)),
                    text.split_last().map(|(_, parent)| parent.to_vec())
                );
                prop_assert!(decl == &whole);
                for probe in probes.iter().chain(&interned).chain([&container, &text]) {
                    prop_assert_eq!(decl.spells(&names, probe), *probe == text, "{:?}", probe);
                    prop_assert_eq!(
                        decl.starts_with(&names, probe),
                        text.starts_with(probe),
                        "{:?}", probe
                    );
                    let other = DeclPath::new(&names, probe);
                    prop_assert_eq!(decl == &other, *probe == text, "{:?}", probe);
                }
                for segment in &segments {
                    prop_assert_eq!(
                        decl.has_segment(&names, segment),
                        text.contains(segment),
                        "{:?}", segment
                    );
                }
                // The questions a lookup asks of an export's value path,
                // each against the expression it replaced.
                for probe in probes.iter().chain(&interned).chain([&container, &text]) {
                    let key = PathKey::of(&names, probe);
                    prop_assert_eq!(decl.is(&names, key), *probe == text, "{:?}", probe);
                    prop_assert_eq!(
                        decl.is_child_of(&names, key),
                        text.len() == probe.len() + 1 && text.starts_with(probe),
                        "{:?}", probe
                    );
                    prop_assert_eq!(
                        decl.is_strictly_under(&names, key),
                        text.len() > probe.len() && text.starts_with(probe),
                        "{:?}", probe
                    );
                }
                for site_text in sites.iter().chain([&container, &text]) {
                    let mut site = ContainerPath::new(&names);
                    site.set(site_text.clone());
                    for root in std::iter::once(None).chain((0..=text.len()).map(Some)) {
                        prop_assert_eq!(
                            decl.accessible_from(&names, root, &site),
                            super::super::model::accessible_from(root, &text, site_text),
                            "{:?} from {:?}", root, site_text
                        );
                    }
                }
            }
        }

        /// An export's name reads back as the text it was published as,
        /// and compares as that text does, whatever else the table holds.
        #[test]
        fn an_export_name_answers_as_its_text_does(
            interned in prop::collection::vec(segment(), 0..6),
            text in segment(),
            other in segment(),
        ) {
            let names = NameTable::default();
            for name in &interned {
                names.logical(name);
            }
            let name = ExportName::new(&names, names.logical(&text), &text);
            prop_assert_eq!(name.text(&names), text.as_str());
            let other_name = ExportName::new(&names, names.logical(&other), &other);
            prop_assert_eq!(name == other_name, text == other);
            let fresh = NameTable::default();
            for name in interned.iter().rev() {
                fresh.logical(name);
            }
            prop_assert_eq!(name.rekey(&names, &fresh).text(&fresh), text.as_str());
        }
    }
}
