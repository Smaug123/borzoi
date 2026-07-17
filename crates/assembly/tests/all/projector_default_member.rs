//! `[System.Reflection.DefaultMemberAttribute]` decoding against the real
//! `MiniLib.dll`, read through the public byte entry point
//! `Ecma335Assembly::parse`. Covers the three shapes a real compiler emits:
//! the auto-emitted `"Item"` marker on an indexer-declaring type, an
//! explicitly hand-applied non-`"Item"` name, and the absence path on a
//! plain type.
//!
//! These are absolute value pins on `Entity::default_member`. The
//! `assembly_diff` differential test cross-checks the same fixtures for
//! agreement with FCS, and `minilib_indexer_projects_index_parameter`
//! there also pins the `"Item"` value incidentally to indexer projection;
//! this file is the focused home for the default-member decode matrix
//! (this file pins the values; the diff pins agreement with FCS).
//!
//! The shapes no C#/F# compiler emits — named args on the attribute and a
//! null ctor arg — have no compiler-produced fixture and stay pinned by the
//! reader's hand-built-blob unit tests in `src/reader/attributes_tests.rs`.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{DefaultMember, Ecma335Assembly, EcmaView, Entity};

use crate::common::ensure_minilib_built;

fn load() -> Vec<Entity> {
    let dll = ensure_minilib_built();
    let bytes = std::fs::read(dll).expect("read MiniLib.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MiniLib");
    view.enumerate_type_defs().expect("enumerate MiniLib types")
}

fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities.iter().find(|e| e.name == name).unwrap_or_else(|| {
        panic!(
            "entity {name:?} not found among {:?}",
            entities.iter().map(|e| &e.name).collect::<Vec<_>>()
        )
    })
}

#[test]
fn indexer_host_decodes_default_member_item() {
    // `public int this[int i] => i;` — Roslyn auto-emits
    // `[DefaultMember("Item")]` on any type declaring an indexer.
    let entities = load();
    assert_eq!(
        entity(&entities, "IndexerHost").default_member,
        Some(DefaultMember::Named("Item".to_string())),
    );
}

#[test]
fn explicit_default_member_attribute_decodes_string_arg() {
    // `[System.Reflection.DefaultMember("CustomThing")]` written by hand —
    // the same decode path as the implicit `"Item"`, just with an
    // arbitrary positional string that is not an indexer name.
    let entities = load();
    assert_eq!(
        entity(&entities, "ExplicitDefaultMember").default_member,
        Some(DefaultMember::Named("CustomThing".to_string())),
    );
}

#[test]
fn entity_without_default_member_attribute_projects_none() {
    // `Counter` declares no indexer and carries no `[DefaultMember]`, so the
    // absence path must project `None` even on a member-heavy type — a stray
    // `Some` would mean the decoder is picking up an unrelated attribute.
    let entities = load();
    assert_eq!(entity(&entities, "Counter").default_member, None);
}
