//! [`DeclPath`]: the path an export declaration names, as an interned
//! [`PathId`] (`docs/sema-interning-plan.md`, Stage 6a); and [`ExportName`],
//! the name an export publishes, as an interned [`Name`] (Stage 6c).

use super::container::ContainerPath;
use crate::names::{Name, NameTable, PathId, PathKey};

/// The qualified path an [`ExportDecl`](super::model::ExportDecl) names, held
/// by its id in the env's [`NameTable`], so a declaration and every index
/// entry derived from it, such as each one
/// [`ProjectItems::extend_with`](super::model::ProjectItems::extend_with)
/// derives, costs one integer, whatever the path's length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeclPath {
    id: PathId,
}

impl DeclPath {
    /// `path`, interned.
    pub(super) fn new<S: AsRef<str>>(names: &NameTable, path: &[S]) -> Self {
        DeclPath {
            id: names.intern_path(path),
        }
    }

    /// The path whose id is `id`.
    pub(super) fn interned(id: PathId) -> Self {
        DeclPath { id }
    }

    /// `container` extended by `segments`, interned.
    pub(super) fn under<S: AsRef<str>>(
        names: &NameTable,
        container: PathId,
        segments: &[S],
    ) -> Self {
        DeclPath {
            id: names.intern_extension(container, segments),
        }
    }

    /// This path extended by `segments`, interned.
    pub(super) fn extended<S: AsRef<str>>(&self, names: &NameTable, segments: &[S]) -> Self {
        DeclPath::under(names, self.id, segments)
    }

    /// The same path, as an id of `to`'s; `self` is `from`'s.
    #[cfg(feature = "test-support")]
    pub(super) fn rekey(&self, from: &NameTable, to: &NameTable) -> Self {
        DeclPath {
            id: crate::names::rekey_path(from, to, self.id),
        }
    }

    /// The path's id.
    pub(super) fn id(&self) -> PathId {
        self.id
    }

    /// The path without its last segment; `None` for the empty path.
    pub(super) fn parent(&self, names: &NameTable) -> Option<DeclPath> {
        names
            .split(self.id)
            .map(|(parent, _)| DeclPath { id: parent })
    }

    /// The path's last segment; `None` for the empty path.
    pub(super) fn last<'n>(&self, names: &'n NameTable) -> Option<&'n str> {
        names.split(self.id).map(|(_, last)| names.text(last))
    }

    /// Whether the path's segments are exactly `path`.
    pub(super) fn spells(&self, names: &NameTable, path: &[String]) -> bool {
        names.lookup_path(path) == Some(self.id)
    }

    /// Whether `prefix` is a prefix of the path, the path itself and the
    /// empty prefix included.
    pub(super) fn starts_with(&self, names: &NameTable, prefix: &[String]) -> bool {
        names
            .lookup_path(prefix)
            .is_some_and(|prefix| names.is_prefix(prefix, self.id))
    }

    /// Whether any of the path's segments is `segment`.
    pub(super) fn has_segment(&self, names: &NameTable, segment: &str) -> bool {
        names
            .lookup(segment)
            .is_some_and(|segment| names.names_inward(self.id).any(|name| name == segment))
    }

    /// The path's segments.
    pub(super) fn spell(&self, names: &NameTable) -> Vec<String> {
        names
            .segments(self.id)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// Whether the path is exactly `path`.
    pub(super) fn is(&self, path: PathKey<'_>) -> bool {
        path.id == Some(self.id)
    }

    /// Whether the path is `parent` extended by one name.
    pub(super) fn is_child_of(&self, names: &NameTable, parent: PathKey<'_>) -> bool {
        parent
            .id
            .is_some_and(|parent| names.split(self.id).is_some_and(|(up, _)| up == parent))
    }

    /// Whether `prefix` is a proper prefix of the path.
    pub(super) fn is_strictly_under(&self, names: &NameTable, prefix: PathKey<'_>) -> bool {
        prefix
            .id
            .is_some_and(|prefix| prefix != self.id && names.is_prefix(prefix, self.id))
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
        access_root_len.is_none_or(|k| {
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
        })
    }
}

/// The name an [`ExportedItem`](super::model::ExportedItem) publishes, in the
/// logical domain (`idText`: backticks stripped, a bare operator compiled),
/// held by its id in the env's [`NameTable`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExportName {
    name: Name,
}

impl ExportName {
    /// The export name `name`.
    pub(super) fn new(name: Name) -> Self {
        ExportName { name }
    }

    /// The name's text.
    pub(super) fn text<'n>(&self, names: &'n NameTable) -> &'n str {
        names.text(self.name)
    }

    /// The same name, as an id of `to`'s; `self` is `from`'s.
    #[cfg(feature = "test-support")]
    pub(super) fn rekey(&self, from: &NameTable, to: &NameTable) -> Self {
        ExportName {
            name: crate::names::rekey_name(from, to, self.name),
        }
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
            let under = DeclPath::under(&names, names.intern_path(&container), &tail);
            let extended = DeclPath::new(&names, &container).extended(&names, &tail);
            for decl in [&whole, &under, &extended] {
                prop_assert_eq!(decl.spell(&names), text.clone());
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
                    prop_assert_eq!(decl.is(key), *probe == text, "{:?}", probe);
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
            let name = ExportName::new(names.logical(&text));
            prop_assert_eq!(name.text(&names), text.as_str());
            let other_name = ExportName::new(names.logical(&other));
            prop_assert_eq!(name == other_name, text == other);
            let fresh = NameTable::default();
            for name in interned.iter().rev() {
                fresh.logical(name);
            }
            prop_assert_eq!(name.rekey(&names, &fresh).text(&fresh), text.as_str());
        }
    }
}
