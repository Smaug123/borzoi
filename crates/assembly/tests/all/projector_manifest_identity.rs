//! The manifest identities a binder matches beyond simple name: which names a
//! manifest qualifies by culture or flags, and the whole `AssemblyRef` identity a type
//! forwarder points at. Asserted against a PE the in-box metadata emitter
//! writes (`tests/fixtures/assembly/MetadataEmitter`, shape
//! `manifest_cultures_and_forwarder`).

use crate::common;

use borzoi_assembly::{AssemblyIdentity, Ecma335Assembly, EcmaView, TypeRef, Version};

#[test]
fn qualified_names_include_the_own_and_referenced_cultured_names() {
    let bytes = common::emit_metadata_fixture("manifest_cultures_and_forwarder");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let mut names = view.qualified_names().expect("the reader reads cultures");
    names.sort();
    assert_eq!(
        names,
        ["CultureFixture", "Localised"],
        "the `fr` manifest and the `de` reference, and not the neutral ones"
    );
}

#[test]
fn a_forwarder_carries_its_assembly_refs_whole_identity() {
    let bytes = common::emit_metadata_fixture("manifest_cultures_and_forwarder");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let forwarders = view.type_forwarders().expect("forwarders");
    let [moved] = forwarders.as_slice() else {
        panic!("one forwarder: {forwarders:?}");
    };
    assert_eq!(
        (moved.namespace.as_str(), moved.name.as_str()),
        ("N", "Moved")
    );
    assert_eq!(
        moved.assembly,
        AssemblyIdentity {
            name: "Target".to_string(),
            version: Version {
                major: 4,
                minor: 2,
                build: 0,
                revision: 0,
            },
            public_key_token: Some([0xb0, 0x3f, 0x5f, 0x7f, 0x11, 0xd5, 0x0a, 0x3a]),
        }
    );
}

/// An `AssemblyRef` spelling the assembly's own name, version and token is
/// a reference to the assembly itself only when its culture is the
/// assembly's too: a culture-qualified one names a satellite, which a binder
/// keeps apart, so the reference keeps its assembly.
#[test]
fn a_self_reference_in_another_culture_keeps_its_assembly() {
    let bytes = common::emit_metadata_fixture("self_reference_cultures");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let entities = view.enumerate_type_defs().expect("project");
    let base_assembly = |name: &str| {
        let e = entities
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no {name}"));
        match &e.base_type {
            Some(TypeRef::Named { assembly, name, .. }) => {
                assert_eq!(name, "Helper");
                assembly.clone()
            }
            other => panic!("{name}: base {other:?}"),
        }
    };
    assert_eq!(base_assembly("Neutral"), None, "the neutral self-reference");
    assert_eq!(
        base_assembly("Localised").map(|a| a.name),
        Some("SelfCultureFixture".to_string()),
        "the `fr` reference names a satellite"
    );
}

/// A binder matches an `AssemblyRef`'s flags too (its content type, whether
/// it is retargetable), which the model does not carry: a name some row of
/// the manifest so flags is qualified, as a cultured one is. `PublicKey`,
/// the processor architecture and the JIT hints qualify nothing.
#[test]
fn a_name_a_row_flags_beyond_its_key_is_qualified() {
    let bytes = common::emit_metadata_fixture("manifest_flagged_identities");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let mut names = view.qualified_names().expect("the reader reads flags");
    names.sort();
    assert_eq!(names, ["FlaggedFixture", "Retarget", "WinRt"]);
}
