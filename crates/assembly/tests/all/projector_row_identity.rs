//! The pickle-to-row join on real fsc output (`RowIdentity`): each pickled
//! entity's overlays land on the `TypeDef` row it compiled to, found by the
//! row's exact metadata identity, never on a row whose name merely strips
//! alike.
//!
//! A row is picked out here by [`Entity::metadata_name`] — the row's own name,
//! suffix and all — so a test says whose row it is reading without trusting
//! the join it is testing.

use borzoi_assembly::{
    AssemblyProjectionSkips, Ecma335Assembly, EcmaView, Entity, EntityKind, JoinVerdict,
    UnionCases,
};

use crate::common::ensure_row_identity_built;

fn assembly() -> Ecma335Assembly {
    let bytes = std::fs::read(ensure_row_identity_built()).expect("read fixture dll");
    Ecma335Assembly::parse(&bytes).expect("parse fixture dll")
}

fn project() -> (Vec<Entity>, AssemblyProjectionSkips) {
    assembly()
        .enumerate_type_defs_with_skips()
        .expect("enumerate fixture types")
}

/// The row whose namespace is `namespace` and whose metadata names, from the
/// outermost row in, are `path`.
fn row<'a>(entities: &'a [Entity], namespace: &[&str], path: &[&str]) -> &'a Entity {
    let (top, nested) = path.split_first().expect("a non-empty path");
    let mut current = entities
        .iter()
        .find(|e| e.namespace == namespace && e.metadata_name.as_deref() == Some(top))
        .unwrap_or_else(|| panic!("no row {namespace:?} {top}"));
    for segment in nested {
        current = current
            .nested_types
            .iter()
            .find(|e| e.metadata_name.as_deref() == Some(segment))
            .unwrap_or_else(|| panic!("no row {segment} in {path:?}"));
    }
    current
}

fn case_names(e: &Entity) -> Option<Vec<&str>> {
    match &e.union_cases {
        UnionCases::Known(cases) => Some(cases.iter().map(|c| c.name.as_str()).collect()),
        UnionCases::Unknowable => None,
    }
}

const NS: &[&str] = &["RowIdentity"];

/// Every row-bearing entity of a single compilation is joined: fsc writes one
/// row per such entity and refuses two rows of one full name, so a verdict
/// other than these would be the join's model of fsc being wrong. The image is
/// therefore authoritative, and the one row the projector drops is recorded.
#[test]
fn every_entity_of_one_compilation_is_joined_to_its_row() {
    let census = assembly()
        .pickle_join_census()
        .expect("read the pickle")
        .expect("the fixture has a host pickle");
    let unjoined: Vec<_> = census
        .verdicts
        .iter()
        .filter(|(v, _)| !matches!(v, JoinVerdict::Joined | JoinVerdict::NoOwnTypeDef))
        .collect();
    assert!(unjoined.is_empty(), "unjoined entities: {unjoined:?}");
    assert!(
        census.verdicts.get(&JoinVerdict::Joined).copied().unwrap_or(0) >= 9,
        "the fixture's types and module are all counted: {census:?}"
    );

    let (_, skips) = project();
    assert!(!skips.fsharp_signature_non_authoritative);
    assert!(
        skips.dropped_types.iter().any(|d| d.name.contains("Dr`1")),
        "precondition: `Dr<'T>`'s row is dropped: {:?}",
        skips.dropped_types
    );
}

/// Each union's cases land on its own row, among rows whose names strip alike.
#[test]
fn every_union_keeps_its_own_cases() {
    let (entities, _) = project();
    for (namespace, path, cases) in [
        (NS, &["V"][..], vec!["X", "Y"]),
        (NS, &["V`0"], vec!["R", "S"]),
        // Beside the dropped ``Dr`1``, whose cases must not move in.
        (NS, &["Dr`2"], vec!["SqA", "SqB"]),
        (&["RowIdentity", "Inner"][..], &["Dotted"], vec!["DA", "DB"]),
        (NS, &["M`1"], vec!["V", "Other"]),
        (NS, &["M", "V"], vec!["MA", "MB"]),
    ] {
        let union = row(&entities, namespace, path);
        assert_eq!(union.kind, EntityKind::Union, "{path:?}");
        assert_eq!(case_names(union), Some(cases), "{path:?}");
    }
    // The case class `V` that fsc nests in ``M`1`` is a namesake of `M.V`, not
    // a union.
    let case_class = row(&entities, NS, &["M`1", "V"]);
    assert_eq!(case_class.union_cases, UnionCases::Unknowable);
}

/// Each renamed type's F# source name lands on its own row.
#[test]
fn every_renamed_type_keeps_its_own_source_name() {
    let (entities, _) = project();
    for (namespace, path, source) in [
        (NS, &["V"][..], None),
        (NS, &["V`0"], Some("W")),
        (NS, &["Dr`2"], Some("Squatter")),
        (&["RowIdentity", "Inner"][..], &["Dotted"], Some("Dotted")),
        (NS, &["M`1"], Some("Q")),
        (NS, &["M"], None),
        (NS, &["M", "V"], None),
    ] {
        let e = row(&entities, namespace, path);
        assert_eq!(e.source_name.as_deref(), source, "{path:?}");
        assert!(e.definition_range.is_some(), "{path:?} has its range");
    }
}
