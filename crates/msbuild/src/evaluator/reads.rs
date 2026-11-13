//! The end of an evaluation as a target reads it: stage A₁.3a of
//! `docs/nuget-dgspec-boundary-plan.md`.
//!
//! The restore targets run on the evaluation MSBuild finishes with. A target
//! reads a property by name and an item list by type, and whatever it writes
//! is only as trustworthy as what it read. So the question this module answers
//! is, for each name a caller asks about, what a read at the end of the walk
//! sees, and whether that is the real build's answer.
//!
//! A [`ReadProbe`] names the properties and item types. The walk answers it
//! from its final state, which [`super::State::into_project`] otherwise drops:
//! the property table with its trust, and the item tables
//! [`super::item_pass`] keeps for types this crate does not otherwise model,
//! with their taint. The probe changes nothing about the walk, so the
//! [`crate::ParsedProject`] beside it is the one the unprobed walk returns.

use std::collections::BTreeMap;

use super::item_pass::{
    helper_item_definition_default_uncertainties, is_package_id_shape, item_list_view,
};
use super::{EvaluatedItem, HelperMetadataUncertainty, HelperMetadataUncertaintyKind};
use super::{State, is_toolset_initial_property_name, item_key};
use crate::{PackageReferenceUncertaintyCauseKind, UntrustedCause};

/// The names a [`ReadProbe`]'s caller wants read at the end of the walk.
#[derive(Debug, Clone, Default)]
pub struct ReadProbe {
    /// Property names, in any case.
    pub properties: Vec<String>,
    /// Item types, in any case.
    pub item_types: Vec<String>,
}

/// What the walk's reads saw, keyed by the names as the probe spelt them.
#[derive(Debug, Clone, Default)]
pub struct EvaluationReads {
    pub properties: BTreeMap<String, PropertyRead>,
    pub items: BTreeMap<String, ItemRead>,
}

/// A read of one property at the end of the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyRead {
    /// A value the walk trusts, unescaped as a target's read sees it.
    Value(String),
    /// Undefined, and undefined in the real build too.
    Unset,
    /// Undefined in the walk, but the real build may define it.
    UnsetInexact(InexactUnset),
    /// A value the walk holds but may have wrong.
    Untrusted(UntrustedCause),
}

/// Why an undefined name might be defined in the real build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InexactUnset {
    /// The walk refused a write of it (a value it could not evaluate), so the
    /// real build holds a value where the walk holds none.
    Refused,
    /// The environment may define it.
    Environment,
    /// MSBuild's toolset defines it before evaluation starts, and the walk
    /// does not model that one.
    Toolset,
    /// The walk passed content it could not see, which may define it.
    Opaque(UntrustedCause),
    /// A name the walk keeps inexact for its own consumers (`DefineConstants`,
    /// and `TargetFramework` in a build that may be a cross-targeting
    /// dispatch).
    ConsumerCarveOut,
}

/// A read of one item type at the end of the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRead {
    /// The list the walk captured, in order, or `None` when it captured no
    /// list of that type (none was declared where it walked, or the type is
    /// one this crate models elsewhere, as `PackageReference` is).
    pub items: Option<Vec<ItemRow>>,
    /// Whether the walk itself would take `items` as MSBuild's list, by the
    /// predicate an `@(…)` reference in the walk uses: no opacity, no taint,
    /// nothing untracked. Only an exact list makes a claim.
    pub exact: bool,
    /// Some declaration of the type could not be fully evaluated, so the list
    /// may be missing entries.
    pub tainted: bool,
    /// MSBuild's list may hold entries the walk never copied into its table.
    pub untracked: bool,
}

/// One captured item: its identity, decoded, and every metadatum its declaration
/// gave it, keyed lowercase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRow {
    pub identity: String,
    pub metadata: BTreeMap<String, String>,
    /// Metadata names whose value the walk does not know, lowercase: one it
    /// could not evaluate, or one an item definition may give a default (which
    /// the walk does not apply).
    pub uncertain_metadata: Vec<String>,
    /// Whether `metadata` and `uncertain_metadata` name every metadatum the
    /// item carries. When false, a name in neither is unknown, not absent.
    pub metadata_complete: bool,
}

impl State<'_> {
    /// Answer `probe` from the walk's final state.
    ///
    /// Run after the walk's observations are taken: the undefined-read check
    /// records a `TargetFramework` exemption as a side effect, which would
    /// otherwise steer [`super::walk_with_imports`]'s choice of pass.
    pub(super) fn read_probe(&self, probe: &ReadProbe) -> EvaluationReads {
        let properties = probe
            .properties
            .iter()
            .map(|name| (name.clone(), self.read_property(name)))
            .collect();
        let items = probe
            .item_types
            .iter()
            .map(|item_type| (item_type.clone(), self.read_items(item_type)))
            .collect();
        EvaluationReads { properties, items }
    }

    fn read_property(&self, name: &str) -> PropertyRead {
        let trust = self.trust_of(name);
        if trust.reference_is_untrusted() {
            let cause = trust
                .untrusted_cause()
                .expect("an untrusted reference has a cause");
            return PropertyRead::Untrusted(cause);
        }
        if let Some(value) = self.lookup.get_unescaped(name) {
            return PropertyRead::Value(value);
        }
        if self.undefined_read_is_exact(name) {
            return PropertyRead::Unset;
        }
        let lower = name.to_ascii_lowercase();
        let why = if trust.refused() {
            InexactUnset::Refused
        } else if self.env_property_names.contains(&lower) {
            InexactUnset::Environment
        } else if is_toolset_initial_property_name(&lower) {
            InexactUnset::Toolset
        } else if let Some(cause) = &self.walk_opaque {
            InexactUnset::Opaque(UntrustedCause::of_opacity(cause))
        } else {
            InexactUnset::ConsumerCarveOut
        };
        PropertyRead::UnsetInexact(why)
    }

    /// Whether the `PackageVersion` list holds what MSBuild's does: no
    /// `Update` or `Remove` of one ran, and every cause that marks the package
    /// set uncertain is either a `PackageVersion`'s existence or one recorded
    /// about another dependency list ([`State::package_causes_elsewhere`]).
    /// A cause raised anywhere else (on a `PackageVersion`, an item group, an
    /// import, an item definition) declines.
    fn package_versions_certain(&self) -> bool {
        !self.package_versions_untracked
            && self
                .package_reference_uncertainties
                .iter()
                .enumerate()
                .all(|(index, cause)| {
                    self.package_causes_elsewhere.contains(&index)
                        || cause.kind == PackageReferenceUncertaintyCauseKind::PackageVersion
                })
    }

    fn read_items(&self, item_type: &str) -> ItemRead {
        let key = item_key(item_type);
        let row = |item: &EvaluatedItem| ItemRow {
            identity: item.identity.clone(),
            metadata: item
                .exact_metadata
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.clone()?)))
                .collect(),
            // A row whose definitions are applied holds them in `exact_metadata`
            // (as `None` where unknown), so a mark that only says "an item
            // definition may set this" says nothing more.
            uncertain_metadata: item
                .metadata_uncertainties
                .iter()
                .filter(|u| {
                    !(item.definitions_applied
                        && u.kind == HelperMetadataUncertaintyKind::ItemDefinitionDefault)
                })
                .map(|u| u.name.to_ascii_lowercase())
                .chain(
                    item.exact_metadata
                        .iter()
                        .filter(|(_, v)| v.is_none())
                        .map(|(k, _)| k.clone()),
                )
                .collect(),
            metadata_complete: item.metadata_complete,
        };
        // `PackageReference`, `FrameworkReference` and `PackageVersion` live in
        // the dependency capture, whose certainty covers their identities:
        // exact when that set is certain and nothing was hidden from the walk.
        // `PackageVersion` marks the set uncertain merely by existing, so its
        // list asks the set's causes instead ([`Self::package_versions_certain`]).
        let dependency = if key == item_key("PackageReference") {
            Some((
                &self.package_reference_rows,
                Some(&self.package_reference_unknown_names),
                !self.package_references_uncertain,
            ))
        } else if key == item_key("FrameworkReference") {
            Some((
                &self.framework_reference_rows,
                None,
                !self.package_references_uncertain,
            ))
        } else if key == item_key("PackageVersion") {
            Some((
                &self.package_version_rows,
                None,
                self.package_versions_certain(),
            ))
        } else {
            None
        };
        if let Some((dependency_rows, unknown_names, certain)) = dependency {
            let rows = dependency_rows
                .iter()
                .map(|item| {
                    let mut item = item.clone();
                    if !item.definitions_applied {
                        item.metadata_uncertainties.extend(
                            helper_item_definition_default_uncertainties(&key, &item, self),
                        );
                    }
                    for name in unknown_names.into_iter().flatten() {
                        item.exact_metadata.remove(name);
                        item.metadata_uncertainties
                            .push(HelperMetadataUncertainty::new(name, "an Update"));
                    }
                    row(&item)
                })
                .collect();
            // An identity outside the package-id shape is one MSBuild may spell
            // otherwise (the Unix path fixup rewrites `\a\b`).
            let spelled_as_written = dependency_rows
                .iter()
                .all(|item| is_package_id_shape(&item.identity));
            return ItemRead {
                items: Some(rows),
                exact: self.walk_opaque.is_none() && certain && spelled_as_written,
                tainted: !certain,
                untracked: false,
            };
        }
        // `ProjectReference` lives in the reference capture. The walk's own
        // `@(ProjectReference)` view decides the list (no uncertain mutation,
        // nothing hidden, every identity spelled as MSBuild spells it); the
        // rows carry what that view leaves out, the metadata.
        if key == item_key("ProjectReference") {
            let rows: Option<Vec<ItemRow>> = self
                .project_references
                .iter()
                .map(|ordered| ordered.row.as_ref().map(row))
                .collect();
            let exact = rows.is_some() && item_list_view(item_type, self).is_some();
            return ItemRead {
                items: rows,
                exact,
                tainted: self.project_references_uncertain
                    || self.project_reference_includes_hidden,
                untracked: false,
            };
        }
        // The list an `@(…)` reference in the walk itself would take as exact,
        // by the same predicate; else the table as captured, for reporting.
        // An identity that keeps a backslash is one whose spelling turns on
        // MSBuild's working directory (`msbuild_spelling`): the walk knows the
        // list, not how MSBuild spells it.
        let view = item_list_view(item_type, self);
        let exact = view
            .as_ref()
            .is_some_and(|items| items.iter().all(|item| !item.identity.contains('\\')));
        // The rows are the table's own: an item declared directly holds its
        // type's definitions already, and only a copy is left with them
        // uncertain. A type with no table reads as the view.
        let items = match self.evaluated_items.get(&key) {
            Some(list) => Some(
                list.iter()
                    .map(|item| {
                        let mut item = item.clone();
                        if !item.definitions_applied {
                            item.metadata_uncertainties.extend(
                                helper_item_definition_default_uncertainties(&key, &item, self),
                            );
                        }
                        row(&item)
                    })
                    .collect(),
            ),
            None => view.map(|items| items.iter().map(row).collect()),
        };
        ItemRead {
            items,
            exact,
            tainted: self.tainted_item_lists.contains(&key),
            untracked: self.untracked_item_lists.contains(&key),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Read `properties` and `item_types` at the end of a walk of `body`, with
    /// imports followed but no SDK or glob resolver, in a directory of its own.
    pub(super) fn read(body: &str, properties: &[&str], item_types: &[&str]) -> EvaluationReads {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.fsproj");
        let source = format!("<Project>{body}</Project>");
        std::fs::write(&path, &source).unwrap();
        let probe = ReadProbe {
            properties: properties.iter().map(|p| (*p).to_owned()).collect(),
            item_types: item_types.iter().map(|i| (*i).to_owned()).collect(),
        };
        let walked = crate::parse_fsproj_probed(
            &source,
            &path,
            &HashMap::new(),
            &HashMap::new(),
            None,
            None,
            Some(&probe),
        )
        .unwrap();
        walked.reads.unwrap()
    }

    #[test]
    fn a_clean_write_reads_as_its_value_and_an_unwritten_name_as_unset() {
        let reads = read(
            "<PropertyGroup><A>x%3By</A><B>$(A)z</B></PropertyGroup>",
            &["A", "b", "Never"],
            &[],
        );
        assert_eq!(reads.properties["A"], PropertyRead::Value("x;y".into()));
        assert_eq!(reads.properties["b"], PropertyRead::Value("x;yz".into()));
        assert_eq!(reads.properties["Never"], PropertyRead::Unset);
    }

    #[test]
    fn a_value_resting_on_an_undecided_condition_reads_as_untrusted() {
        let reads = read(
            "<ItemGroup><I Include=\"a\" /></ItemGroup>\
             <PropertyGroup><A>1</A><A Condition=\"'@(I)' != ''\">2</A></PropertyGroup>",
            &["A"],
            &[],
        );
        assert!(
            matches!(reads.properties["A"], PropertyRead::Untrusted(_)),
            "{:?}",
            reads.properties["A"]
        );
    }

    #[test]
    fn a_refused_write_reads_as_inexact() {
        let reads = read(
            "<ItemGroup><I Include=\"a\" /></ItemGroup>\
             <PropertyGroup><A>@(I)</A></PropertyGroup>",
            &["A"],
            &[],
        );
        assert_eq!(
            reads.properties["A"],
            PropertyRead::UnsetInexact(InexactUnset::Refused)
        );
    }

    #[test]
    fn an_item_list_reads_with_its_identities_and_its_taint() {
        let reads = read(
            "<ItemGroup><H Include=\"a;b%3Bc\" /><T Include=\"x\" /><T Include=\"@(H->'%(F)')\" /></ItemGroup>",
            &[],
            &["h", "T", "Absent"],
        );
        let identities = |read: &ItemRead| -> Vec<String> {
            read.items
                .as_ref()
                .unwrap()
                .iter()
                .map(|r| r.identity.clone())
                .collect()
        };
        assert_eq!(identities(&reads.items["h"]), ["a", "b;c"]);
        assert!(!reads.items["h"].tainted);
        assert_eq!(identities(&reads.items["T"]), ["x"]);
        assert!(reads.items["T"].tainted);
        // No declaration, no taint, no opacity: exactly empty.
        assert_eq!(
            reads.items["Absent"],
            ItemRead {
                items: Some(Vec::new()),
                exact: true,
                tainted: false,
                untracked: false
            }
        );
        assert!(!reads.items["T"].exact);
        assert!(reads.items["h"].exact);
    }
}

#[cfg(test)]
mod package_reference_tests {
    use super::tests::read;

    #[test]
    fn an_update_collapses_onto_the_rows() {
        let reads = read(
            "<ItemGroup><PackageReference Include=\"a\" Version=\"1\" Pack=\"x\" />\
             <PackageReference Update=\"a\" Version=\"2\" Pack=\"y\" /></ItemGroup>",
            &[],
            &["PackageReference"],
        );
        let read = &reads.items["PackageReference"];
        assert!(read.exact);
        let row = &read.items.as_ref().unwrap()[0];
        assert_eq!(row.metadata.get("pack").map(String::as_str), Some("y"));
        assert_eq!(row.metadata.get("version").map(String::as_str), Some("2"));
    }
}

#[cfg(test)]
mod project_reference_tests {
    use super::tests::read;

    #[test]
    fn a_reference_reads_with_its_metadata_and_definitions() {
        let reads = read(
            "<ItemDefinitionGroup><ProjectReference><Flavor>d</Flavor><Pack>d</Pack></ProjectReference></ItemDefinitionGroup>\
             <ItemGroup><ProjectReference Include=\"a.fsproj;b.fsproj\" PrivateAssets=\" all \">\
             <Pack>p</Pack></ProjectReference></ItemGroup>",
            &[],
            &["ProjectReference"],
        );
        let read = &reads.items["ProjectReference"];
        let rows = read.items.as_ref().unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row.metadata["privateassets"], " all ");
            assert_eq!(row.metadata["pack"], "p");
            assert_eq!(row.metadata["flavor"], "d");
            assert!(row.metadata_complete);
            assert!(row.uncertain_metadata.is_empty(), "{row:?}");
        }
        // An item definition for the type makes the reference list uncertain.
        assert!(!read.exact);
    }

    #[test]
    fn a_plain_reference_list_is_exact_and_an_update_makes_it_not() {
        let plain = read(
            "<ItemGroup><ProjectReference Include=\"a.fsproj\" Flavor=\"x\" /></ItemGroup>",
            &[],
            &["ProjectReference"],
        );
        let read_plain = &plain.items["ProjectReference"];
        assert!(read_plain.exact, "{read_plain:?}");
        assert_eq!(
            read_plain.items.as_ref().unwrap()[0].metadata["flavor"],
            "x"
        );
        let updated = read(
            "<ItemGroup><ProjectReference Include=\"a.fsproj\" />\
             <ProjectReference Update=\"a.fsproj\" Flavor=\"y\" /></ItemGroup>",
            &[],
            &["ProjectReference"],
        );
        assert!(!updated.items["ProjectReference"].exact);
    }
}

#[cfg(test)]
mod package_version_tests {
    use super::tests::read;

    #[test]
    fn central_versions_are_exact_beside_an_uncertain_package_reference() {
        // The versionless reference marks the package set uncertain; it says
        // nothing about the central versions.
        let reads = read(
            "<ItemGroup><PackageVersion Include=\"a\" Version=\" 1.0 \" Pin=\"x\" />\
             <PackageReference Include=\"a\" /></ItemGroup>",
            &[],
            &["PackageVersion", "PackageReference"],
        );
        let versions = &reads.items["PackageVersion"];
        assert!(versions.exact, "{versions:?}");
        let row = &versions.items.as_ref().unwrap()[0];
        assert_eq!(row.metadata["version"], " 1.0 ");
        assert_eq!(row.metadata["pin"], "x");
        assert!(!reads.items["PackageReference"].exact);
    }

    #[test]
    fn a_removed_or_updated_central_version_is_not_exact() {
        for mutation in ["Remove=\"a\"", "Update=\"a\" Version=\"2\""] {
            let reads = read(
                &format!(
                    "<ItemGroup><PackageVersion Include=\"a\" Version=\"1\" />\
                     <PackageVersion {mutation} /></ItemGroup>"
                ),
                &[],
                &["PackageVersion"],
            );
            assert!(!reads.items["PackageVersion"].exact, "{mutation}");
        }
    }

    #[test]
    fn a_central_version_under_an_undecidable_condition_is_not_exact() {
        let reads = read(
            "<ItemGroup><PackageVersion Include=\"a\" Version=\"1\" \
             Condition=\"$([System.String]::IsNullOrWhiteSpace(''))\" /></ItemGroup>",
            &[],
            &["PackageVersion"],
        );
        assert!(!reads.items["PackageVersion"].exact);
    }
}

#[cfg(test)]
mod dependency_list_tests {
    use super::tests::read;

    /// MSBuild matches an `Exclude` by normalised path.
    #[test]
    fn an_exclude_naming_the_reference_by_path_removes_it() {
        for item_type in ["PackageVersion", "PackageReference", "FrameworkReference"] {
            let reads = read(
                &format!(
                    "<ItemGroup><{item_type} Include=\"a;b\" Version=\"1\" Exclude=\"./a\" /></ItemGroup>"
                ),
                &[],
                &[item_type],
            );
            let read = &reads.items[item_type];
            let identities: Vec<&str> = read
                .items
                .as_ref()
                .unwrap()
                .iter()
                .map(|r| r.identity.as_str())
                .collect();
            assert_eq!(identities, ["b"], "{item_type}");
        }
    }

    /// A copy of a list this crate keeps in its own bucket reads nothing the
    /// bucket holds, so it cannot be exact.
    #[test]
    fn a_copy_of_a_bucketed_list_is_not_exact() {
        let reads = read(
            "<ItemGroup><PackageReference Include=\"a\" Version=\"1\" \
             Condition=\"$([System.String]::IsNullOrWhiteSpace(''))\" />\
             <PackageVersion Include=\"@(PackageReference)\" /></ItemGroup>",
            &[],
            &["PackageVersion"],
        );
        assert!(!reads.items["PackageVersion"].exact);
    }
}
