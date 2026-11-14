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
use std::ops::Range;
use std::path::PathBuf;

use super::item_pass::{
    PackageItemKind, helper_item_definition_default_uncertainties, is_package_id_shape,
    item_list_view,
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
    /// Target names, in any case.
    pub targets: Vec<String>,
    /// Targets to run on the evaluation, and what to read after
    /// (`evaluator::executor`).
    pub execute: Option<super::executor::ExecutionProbe>,
}

/// What the walk's reads saw, keyed by the names as the probe spelt them.
#[derive(Debug, Clone, Default)]
pub struct EvaluationReads {
    pub properties: BTreeMap<String, PropertyRead>,
    pub items: BTreeMap<String, ItemRead>,
    pub targets: BTreeMap<String, TargetRead>,
    /// The run [`ReadProbe::execute`] asked for, if it asked.
    pub execution: Option<super::executor::ExecutionRead>,
}

/// The definition of one target a build would run: the last `<Target>` of
/// that name, compared case-insensitively, in the order the walk reached the
/// files of the import graph. MSBuild keeps that one and drops the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRead {
    /// The element, or `None` when the walk reached no target of that name.
    pub definition: Option<TargetDefinition>,
    /// Whether the walk saw every file MSBuild's evaluation imports, so that
    /// no definition it did not reach can override `definition`. Only an
    /// exact read makes a claim.
    pub exact: bool,
}

/// Which `Directory.Build.*` files the walk spliced in itself
/// ([`super::walk_directory_build_file`]), standing in for an import
/// `Microsoft.Common.props` or `.targets` makes, and whether the props splice
/// first ran at `Microsoft.Common.props`'s import point and was what that
/// import takes.
#[derive(Debug, Clone, Default)]
pub(super) struct DirectoryBuildSplices {
    pub(super) props_spliced: bool,
    pub(super) targets_spliced: bool,
    pub(super) props_point_reached: bool,
    /// A props import point the walk suppressed although MSBuild's import
    /// there may take a file the splice does not stand for: undecided, or a
    /// file taken again or other than the splice's (a `Directory.Build.props`
    /// can redirect the path, an SDK can import it with the splice off).
    pub(super) props_point_unaccounted: bool,
}

/// Where a `<Target>` element is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDefinition {
    /// The file, spelt as the walk reached it.
    pub file: PathBuf,
    /// The element's byte range in that file's text.
    pub range: Range<usize>,
    /// The element's text, as the file spells it.
    pub xml: String,
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
    /// The identity as MSBuild holds it, still escaped
    /// (`EvaluatedIncludeEscaped`: `a%2Ex` where `identity` reads `a.x`), or
    /// `None` where the walk does not know it.
    pub escaped_identity: Option<String>,
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
        let targets = probe
            .targets
            .iter()
            .map(|name| (name.clone(), self.read_target(name)))
            .collect();
        let execution = probe
            .execute
            .as_ref()
            .map(|execute| super::executor::execute(self, execute));
        EvaluationReads {
            properties,
            items,
            targets,
            execution,
        }
    }

    /// MSBuild keeps the last definition of a name, comparing names unescaped
    /// and `OrdinalIgnoreCase`. Within [`plain_target_name`] that comparison
    /// is ASCII case folding; a name outside it (a `%` escape, whitespace,
    /// non-ASCII) is one this does not compare, so any such definition, or
    /// such a query, declines.
    ///
    /// The target graph is the files the walk reached. It is MSBuild's when
    /// the walk is not opaque and every `Directory.Build.*` file it spliced
    /// in itself stands for an import an SDK chain really makes. The props
    /// splice must have run at `Microsoft.Common.props`'s import point and
    /// been what that import takes ([`DirectoryBuildSplices`]). The targets
    /// splice is a fallback the walk makes only when no chain reached its
    /// import point first, so MSBuild never makes it.
    /// Otherwise whatever the file defines or steers is the walk's alone.
    pub(super) fn read_target(&self, name: &str) -> TargetRead {
        let definition = self
            .target_definitions
            .iter()
            .rev()
            .find(|(defined, _)| defined.eq_ignore_ascii_case(name))
            .map(|(_, definition)| definition.clone());
        let comparable = plain_target_name(name)
            && self
                .target_definitions
                .iter()
                .all(|(defined, _)| plain_target_name(defined));
        let splices = &self.directory_build_splices;
        let graph_is_msbuilds = self.walk_opaque.is_none()
            && !splices.props_point_unaccounted
            && (!splices.props_spliced || splices.props_point_reached)
            && !splices.targets_spliced;
        TargetRead {
            definition,
            exact: comparable && graph_is_msbuilds,
        }
    }

    pub(super) fn read_property(&self, name: &str) -> PropertyRead {
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

    /// Whether the package set's causes leave the central-package list of
    /// `kind` (`PackageVersion` or `GlobalPackageReference`) as MSBuild holds
    /// it: every cause is about another dependency list
    /// ([`State::package_cause_owners`]), or is `marker`, which an item of
    /// `kind` records merely by existing. A cause with no owner (an item group,
    /// an import, an item definition), or any other cause about this list,
    /// declines. The caller checks for an `Update` or `Remove` of the type.
    fn central_list_certain(
        &self,
        kind: PackageItemKind,
        marker: &PackageReferenceUncertaintyCauseKind,
    ) -> bool {
        self.package_reference_uncertainties
            .iter()
            .enumerate()
            .all(
                |(index, cause)| match self.package_cause_owners.get(&index) {
                    Some(owner) if *owner != kind => true,
                    Some(_) => cause.kind == *marker,
                    None => false,
                },
            )
    }

    pub(super) fn read_items(&self, item_type: &str) -> ItemRead {
        let key = item_key(item_type);
        let row = |item: &EvaluatedItem| ItemRow {
            identity: item.identity.clone(),
            escaped_identity: item.escaped_identity.clone(),
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
        // The dependency types live in the dependency capture, whose certainty
        // covers their identities: exact when that set is certain and nothing
        // was hidden from the walk. A central-package item marks the set
        // uncertain merely by existing, so its list asks the set's causes
        // instead ([`Self::central_list_certain`]).
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
                !self.package_versions_untracked
                    && self.central_list_certain(
                        PackageItemKind::PackageVersion,
                        &PackageReferenceUncertaintyCauseKind::PackageVersion,
                    ),
            ))
        } else if key == item_key("GlobalPackageReference") {
            Some((
                &self.global_package_reference_rows,
                None,
                !self.global_package_references_mutated
                    && self.central_list_certain(
                        PackageItemKind::GlobalPackageReference,
                        &PackageReferenceUncertaintyCauseKind::GlobalPackageReference,
                    ),
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

/// A target name MSBuild's comparison (unescape, then `OrdinalIgnoreCase`)
/// treats as ASCII case folding: non-empty, printable ASCII, and free of `%`
/// and whitespace.
fn plain_target_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic() && b != b'%')
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
            targets: Vec::new(),
            execute: None,
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

#[cfg(test)]
mod global_package_reference_tests {
    use super::tests::read;

    #[test]
    fn global_references_are_exact_beside_uncertain_central_versions() {
        // A `PackageVersion` under an undecidable condition says nothing about
        // the global references, nor they about it.
        let reads = read(
            "<ItemGroup><GlobalPackageReference Include=\"g\" Version=\"1\" Extra=\"e\" />\
             <PackageVersion Include=\"a\" Version=\"1\" \
             Condition=\"$([System.String]::IsNullOrWhiteSpace(''))\" /></ItemGroup>",
            &[],
            &["GlobalPackageReference", "PackageVersion"],
        );
        let globals = &reads.items["GlobalPackageReference"];
        assert!(globals.exact, "{globals:?}");
        assert_eq!(globals.items.as_ref().unwrap()[0].metadata["extra"], "e");
        assert!(!reads.items["PackageVersion"].exact);
    }

    #[test]
    fn an_updated_global_reference_is_not_exact() {
        let reads = read(
            "<ItemGroup><GlobalPackageReference Include=\"g\" Version=\"1\" />\
             <GlobalPackageReference Remove=\"g\" /></ItemGroup>",
            &[],
            &["GlobalPackageReference"],
        );
        assert!(!reads.items["GlobalPackageReference"].exact);
    }
}

#[cfg(test)]
mod target_tests {
    use std::collections::HashMap;

    use super::*;

    /// Read `targets` at the end of a walk of `body`, which may import
    /// `t.targets` holding `imported`.
    fn read_targets(
        body: &str,
        imported: &str,
        targets: &[&str],
    ) -> (EvaluationReads, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.fsproj");
        let source = format!("<Project>{body}</Project>");
        std::fs::write(&path, &source).unwrap();
        std::fs::write(
            dir.path().join("t.targets"),
            format!("<Project>{imported}</Project>"),
        )
        .unwrap();
        let probe = ReadProbe {
            targets: targets.iter().map(|t| (*t).to_owned()).collect(),
            ..ReadProbe::default()
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
        (walked.reads.unwrap(), dir)
    }

    #[test]
    fn the_last_definition_across_the_import_graph_wins() {
        const IMPORTED: &str = "<Target Name=\"A\" /><Target Name=\"B\" />";
        let (reads, dir) = read_targets(
            "<Target Name=\"a\" /><Import Project=\"t.targets\" /><Target Name=\"B\" DependsOnTargets=\"A\" />",
            IMPORTED,
            &["A", "b", "Absent"],
        );
        let a = &reads.targets["A"];
        assert!(a.exact);
        let a = a.definition.as_ref().unwrap();
        assert_eq!(a.file, dir.path().join("t.targets"));
        let imported = format!("<Project>{IMPORTED}</Project>");
        assert_eq!(&imported[a.range.clone()], "<Target Name=\"A\" />");
        let b = reads.targets["b"].definition.as_ref().unwrap();
        assert_eq!(b.file, dir.path().join("p.fsproj"));
        assert!(reads.targets["Absent"].exact);
        assert_eq!(reads.targets["Absent"].definition, None);
    }

    #[test]
    fn an_opaque_walk_makes_no_claim() {
        let (reads, _) = read_targets(
            "<Import Project=\"t.targets\" Condition=\"$([System.String]::IsNullOrWhiteSpace(''))\" />",
            "<Target Name=\"A\" />",
            &["A"],
        );
        assert!(!reads.targets["A"].exact);
    }
}

#[cfg(test)]
mod spliced_target_tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{SdkPaths, SdkResolution};

    /// Read target `Phantom` at the end of a walk of an SDK project whose
    /// `Sdk.props` and `Sdk.targets` bodies are given, beside a
    /// `Directory.Build.props` that defines it.
    pub(super) fn read_phantom(sdk_props: &str, sdk_targets: &str) -> TargetRead {
        read_phantom_beside(
            sdk_props,
            sdk_targets,
            "<Project><Target Name=\"Phantom\" /></Project>",
            None,
        )
    }

    /// [`read_phantom`] beside a `Directory.Build.props` of `beside`, and an
    /// `other.props` of `other` if given.
    pub(super) fn read_phantom_beside(
        sdk_props: &str,
        sdk_targets: &str,
        beside: &str,
        other: Option<&str>,
    ) -> TargetRead {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::observed::host_fs::canonicalize(dir.path()).unwrap();
        let sdk = root.join("sdk");
        std::fs::create_dir(&sdk).unwrap();
        std::fs::write(sdk.join("Sdk.props"), sdk_props).unwrap();
        std::fs::write(sdk.join("Sdk.targets"), sdk_targets).unwrap();
        let project = root.join("p").join("p.fsproj");
        std::fs::create_dir(project.parent().unwrap()).unwrap();
        std::fs::write(root.join("p").join("Directory.Build.props"), beside).unwrap();
        if let Some(other) = other {
            std::fs::write(root.join("p").join("other.props"), other).unwrap();
        }
        let source = "<Project Sdk=\"S\"></Project>";
        std::fs::write(&project, source).unwrap();
        let resolver = |_: &str, _: &crate::observed::Observer| {
            Ok(SdkResolution::from(SdkPaths {
                root: sdk.clone(),
                props: sdk.join("Sdk.props"),
                targets: sdk.join("Sdk.targets"),
            }))
        };
        let probe = ReadProbe {
            targets: vec!["Phantom".to_owned()],
            ..ReadProbe::default()
        };
        let walked = crate::parse_fsproj_probed(
            source,
            &project,
            &HashMap::new(),
            &HashMap::new(),
            Some(&resolver),
            None,
            Some(&probe),
        )
        .unwrap();
        walked.reads.unwrap().targets["Phantom"].clone()
    }

    /// An SDK whose `Sdk.targets` reaches the `Directory.Build.targets` import
    /// point but whose `Sdk.props` never reaches the props one: MSBuild
    /// imports no `Directory.Build.props`, so the walk's splice of it decides
    /// nothing.
    #[test]
    fn a_props_splice_no_chain_asked_for_declines() {
        let read = read_phantom(
            "<Project />",
            "<Project><Import Project=\"$(DirectoryBuildTargetsPath)\" \
             Condition=\"Exists('$(DirectoryBuildTargetsPath)')\" /></Project>",
        );
        assert!(read.definition.is_some());
        assert!(!read.exact, "{read:?}");
    }

    /// The same SDK with the props import point, as `Microsoft.Common.props`
    /// writes it, path first: the splice stands for a real import.
    #[test]
    fn a_props_splice_at_a_reached_import_point_is_exact() {
        let read = read_phantom(
            "<Project><PropertyGroup><DirectoryBuildPropsPath>\
             $([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildProjectDirectory)'))\
             </DirectoryBuildPropsPath></PropertyGroup>\
             <Import Project=\"$(DirectoryBuildPropsPath)\" \
             Condition=\"Exists('$(DirectoryBuildPropsPath)')\" /></Project>",
            "<Project />",
        );
        assert!(read.definition.is_some());
        assert!(read.exact, "{read:?}");
    }
}

#[cfg(test)]
mod spliced_target_condition_tests {
    use super::spliced_target_tests::read_phantom;

    /// The props import point is reached, but its condition is false, so
    /// MSBuild imports nothing there.
    #[test]
    fn a_props_import_point_whose_condition_is_false_declines() {
        let read = read_phantom(
            "<Project><Import Project=\"$(DirectoryBuildPropsPath)\" \
             Condition=\"false And Exists('$(DirectoryBuildPropsPath)')\" /></Project>",
            "<Project />",
        );
        assert!(!read.exact, "{read:?}");
    }
}

#[cfg(test)]
mod spliced_target_order_tests {
    use super::spliced_target_tests::read_phantom_beside;

    /// The import's condition is false until the spliced file runs: MSBuild
    /// decides it first and imports nothing.
    #[test]
    fn a_props_import_decided_before_the_splice_runs_declines() {
        let read = read_phantom_beside(
            "<Project><PropertyGroup><Gate>false</Gate></PropertyGroup>\
             <Import Project=\"$(DirectoryBuildPropsPath)\" \
             Condition=\"'$(Gate)' == 'true' And Exists('$(DirectoryBuildPropsPath)')\" /></Project>",
            "<Project />",
            "<Project><PropertyGroup><Gate>true</Gate></PropertyGroup>\
             <Target Name=\"Phantom\" /></Project>",
            None,
        );
        assert!(!read.exact, "{read:?}");
    }
}

#[cfg(test)]
mod spliced_target_redirect_tests {
    use super::spliced_target_tests::read_phantom_beside;

    /// `Directory.Build.props` redirects the path and the SDK reaches a
    /// second props import point: MSBuild takes it, and the walk suppresses
    /// it.
    #[test]
    fn a_second_props_import_point_declines() {
        const POINT: &str = "<Import Project=\"$(DirectoryBuildPropsPath)\" \
             Condition=\"Exists('$(DirectoryBuildPropsPath)')\" />";
        let read = read_phantom_beside(
            &format!(
                "<Project><PropertyGroup><DirectoryBuildPropsPath>\
                 $([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildProjectDirectory)'))\
                 </DirectoryBuildPropsPath></PropertyGroup>{POINT}{POINT}</Project>"
            ),
            "<Project />",
            "<Project><PropertyGroup><DirectoryBuildPropsPath>$(MSBuildThisFileDirectory)other.props\
             </DirectoryBuildPropsPath></PropertyGroup><Target Name=\"Phantom\" /></Project>",
            Some("<Project><Target Name=\"Phantom\" /></Project>"),
        );
        assert!(!read.exact, "{read:?}");
    }
}

#[cfg(test)]
mod suppressed_props_import_tests {
    use super::spliced_target_tests::read_phantom;

    /// The splice is off (`ImportDirectoryBuildProps=false`), but the SDK
    /// imports the file itself: MSBuild takes it, and the walk suppresses it.
    #[test]
    fn an_sdk_import_of_the_props_file_with_the_splice_off_declines() {
        let read = read_phantom(
            "<Project><PropertyGroup><ImportDirectoryBuildProps>false</ImportDirectoryBuildProps>\
             <DirectoryBuildPropsPath>\
             $([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildProjectDirectory)'))\
             </DirectoryBuildPropsPath></PropertyGroup>\
             <Import Project=\"$(DirectoryBuildPropsPath)\" \
             Condition=\"Exists('$(DirectoryBuildPropsPath)')\" /></Project>",
            "<Project />",
        );
        assert!(!read.exact, "{read:?}");
    }
}
