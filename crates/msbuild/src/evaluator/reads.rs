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

use super::EvaluatedItem;
use super::item_pass::{helper_item_definition_default_uncertainties, item_list_view};
use super::{State, is_toolset_initial_property_name, item_key};
use crate::UntrustedCause;

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

    fn read_items(&self, item_type: &str) -> ItemRead {
        let key = item_key(item_type);
        let row = |item: &EvaluatedItem| ItemRow {
            identity: item.identity.clone(),
            metadata: item
                .exact_metadata
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.clone()?)))
                .collect(),
            uncertain_metadata: item
                .metadata_uncertainties
                .iter()
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
        // The list an `@(…)` reference in the walk itself would take as exact,
        // by the same predicate; else the table as captured, for reporting.
        let view = item_list_view(item_type, self);
        let exact = view.is_some();
        let items = match view {
            Some(items) => Some(items.iter().map(row).collect()),
            None => self.evaluated_items.get(&key).map(|list| {
                list.iter()
                    .map(|item| {
                        let mut item = item.clone();
                        item.metadata_uncertainties.extend(
                            helper_item_definition_default_uncertainties(&key, &item, self),
                        );
                        row(&item)
                    })
                    .collect()
            }),
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
    fn read(body: &str, properties: &[&str], item_types: &[&str]) -> EvaluationReads {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.fsproj");
        let source = format!("<Project>{body}</Project>");
        std::fs::write(&path, &source).unwrap();
        let probe = ReadProbe {
            properties: properties.iter().map(|p| (*p).to_owned()).collect(),
            item_types: item_types.iter().map(|i| (*i).to_owned()).collect(),
        };
        let (_, reads) = crate::parse_fsproj_probed(
            &source,
            &path,
            &HashMap::new(),
            &HashMap::new(),
            None,
            None,
            Some(&probe),
        )
        .unwrap();
        reads.unwrap()
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
