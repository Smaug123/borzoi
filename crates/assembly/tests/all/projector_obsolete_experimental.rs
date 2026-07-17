//! `[Obsolete]` / `[Experimental]` payload decoding against the real
//! `MemberShapes.dll`, read through the public byte entry point
//! `Ecma335Assembly::parse`. Covers the ctor/named-arg shapes a C# compiler
//! actually emits: the three `ObsoleteAttribute` ctor overloads (bare,
//! message, message+error) and the full `ExperimentalAttribute` payload
//! matrix (diagnostic id, with `UrlFormat`, with `Message`, with both), in
//! both type and method position.
//!
//! The shapes Roslyn can't emit — named-arg precedence over a get-only
//! property (`Obsolete.Message`/`IsError`, `Experimental.DiagnosticId`) — have
//! no compiler-produced fixture and stay pinned by the reader's
//! hand-built-blob unit tests in `src/reader/attributes_tests.rs`.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use borzoi_assembly::{
    Ecma335Assembly, EcmaView, Entity, Experimental, Member, MethodLike, Obsolete,
};

use crate::common::ensure_member_shapes_built;

fn load() -> Vec<Entity> {
    let dll = ensure_member_shapes_built();
    let bytes = std::fs::read(dll).expect("read MemberShapes.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse MemberShapes");
    view.enumerate_type_defs()
        .expect("enumerate MemberShapes types")
}

fn entity<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    entities.iter().find(|e| e.name == name).unwrap_or_else(|| {
        panic!(
            "entity {name:?} not found among {:?}",
            entities.iter().map(|e| &e.name).collect::<Vec<_>>()
        )
    })
}

fn method<'a>(e: &'a Entity, name: &str) -> &'a MethodLike {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.name == name => Some(m),
            _ => None,
        })
        .unwrap_or_else(|| panic!("method {name:?} not found on {:?}", e.name))
}

#[test]
fn obsolete_bare_decodes_no_payload() {
    // `[Obsolete]`: no ctor args → presence with empty payload.
    let entities = load();
    assert_eq!(
        entity(&entities, "ObsoleteBare").obsolete,
        Some(Obsolete {
            message: None,
            is_error: false,
        }),
    );
}

#[test]
fn obsolete_message_decodes_from_string_ctor() {
    // `[Obsolete("…")]`: the `(string)` ctor arg lifts to `message`,
    // `is_error` stays false.
    let entities = load();
    assert_eq!(
        entity(&entities, "ObsoleteWarned").obsolete,
        Some(Obsolete {
            message: Some("use V2 instead".into()),
            is_error: false,
        }),
    );
}

#[test]
fn obsolete_message_and_error_decode_from_string_bool_ctor() {
    // `[Obsolete("…", true)]`: the `(string, bool)` ctor lifts both args.
    let entities = load();
    assert_eq!(
        entity(&entities, "ObsoleteErrored").obsolete,
        Some(Obsolete {
            message: Some("gone".into()),
            is_error: true,
        }),
    );
}

#[test]
fn obsolete_on_method_projects_onto_method_like() {
    // Obsolete on a method lands on `MethodLike::obsolete`, the second
    // `detect_obsolete_*` call site — distinct from `Entity::obsolete`.
    let entities = load();
    let host = entity(&entities, "ObsoleteOnMethod");
    assert_eq!(host.obsolete, None, "the type itself is not obsolete");
    assert_eq!(
        method(host, "Old").obsolete,
        Some(Obsolete {
            message: Some("use New".into()),
            is_error: false,
        }),
    );
}

#[test]
fn experimental_diagnostic_id_decodes_from_string_ctor() {
    // `[Experimental("DIAG001")]`: the mandatory `(string)` ctor arg lifts
    // to `diagnostic_id`; the optional named properties stay None.
    let entities = load();
    assert_eq!(
        entity(&entities, "ExperimentalBare").experimental,
        Some(Experimental {
            diagnostic_id: Some("DIAG001".into()),
            url_format: None,
            message: None,
        }),
    );
}

#[test]
fn experimental_url_format_named_property_decodes() {
    // ctor + `UrlFormat` named property.
    let entities = load();
    assert_eq!(
        entity(&entities, "ExperimentalWithUrl").experimental,
        Some(Experimental {
            diagnostic_id: Some("DIAG002".into()),
            url_format: Some("https://aka.ms/{0}".into()),
            message: None,
        }),
    );
}

#[test]
fn experimental_message_named_property_decodes() {
    // ctor + `Message` named property.
    let entities = load();
    assert_eq!(
        entity(&entities, "ExperimentalWithMessage").experimental,
        Some(Experimental {
            diagnostic_id: Some("DIAG003".into()),
            url_format: None,
            message: Some("subject to change".into()),
        }),
    );
}

#[test]
fn experimental_both_named_properties_decode() {
    // ctor + both `UrlFormat` and `Message` named properties → all three
    // fields Some.
    let entities = load();
    assert_eq!(
        entity(&entities, "ExperimentalBoth").experimental,
        Some(Experimental {
            diagnostic_id: Some("DIAG004".into()),
            url_format: Some("u".into()),
            message: Some("m".into()),
        }),
    );
}

#[test]
fn experimental_on_method_projects_onto_method_like() {
    // Experimental on a method lands on `MethodLike::experimental`, the
    // second `detect_experimental_*` call site — sibling of
    // `obsolete_on_method_projects_onto_method_like`.
    let entities = load();
    let host = entity(&entities, "ExperimentalOnMethod");
    assert_eq!(
        host.experimental, None,
        "the type itself is not experimental"
    );
    assert_eq!(
        method(host, "Preview").experimental,
        Some(Experimental {
            diagnostic_id: Some("DIAG_M001".into()),
            url_format: None,
            message: None,
        }),
    );
}
