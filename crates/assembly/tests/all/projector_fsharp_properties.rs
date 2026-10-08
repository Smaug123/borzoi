//! The member properties of F# records, exceptions and unions: candidates the
//! projection keeps and the host pickle's published member list settles
//! (`settle_record_properties`, `apply_union_cases`).
//!
//! A user-defined member property is a member FCS surfaces — the fcs-dump
//! differential over MiniLibFs pins that (`Point.Sum`, `Detailed.Doubled`). This
//! file pins the case the differential cannot carry: a `[<CompiledName>]`-renamed
//! property. fsc renames the IL property but not its accessors, so two renamed
//! properties can trade IL names, and the pickle — which publishes accessor
//! names — would vouch for each under the other's name and type. The projection
//! refuses such a property and records the refusal.

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Member};

use crate::common::ensure_renamed_members_built;

fn entity<'a>(types: &'a [Entity], name: &str) -> &'a Entity {
    types
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no entity {name}"))
}

fn property_names(e: &Entity) -> Vec<&str> {
    let mut names: Vec<&str> = e
        .members
        .iter()
        .filter_map(|m| match m {
            Member::Property(p) => Some(p.name.as_str()),
            _ => None,
        })
        .collect();
    names.sort();
    names
}

fn skipped_names(e: &Entity) -> Vec<&str> {
    let mut names: Vec<&str> = e.skipped_members.iter().map(|s| s.name.as_str()).collect();
    names.sort();
    names
}

#[test]
fn renamed_record_and_union_properties_are_refused_not_swapped() {
    let bytes = std::fs::read(ensure_renamed_members_built()).expect("read RenamedMembers.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse RenamedMembers.dll");
    let types = view
        .enumerate_type_defs()
        .expect("enumerate RenamedMembers");

    let swapped = entity(&types, "Swapped");
    assert_eq!(property_names(swapped), vec!["Plain"]);
    assert_eq!(skipped_names(swapped), vec!["First", "Second"]);

    let coin = entity(&types, "Coin");
    assert!(
        !property_names(coin).contains(&"Other") && !property_names(coin).contains(&"Renamed"),
        "{:?}",
        property_names(coin)
    );
    assert_eq!(skipped_names(coin), vec!["Other"]);
}
