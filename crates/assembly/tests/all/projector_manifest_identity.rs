//! The manifest identities a binder matches beyond simple name: which names a
//! manifest qualifies by culture, and the whole `AssemblyRef` identity a type
//! forwarder points at. Asserted against a PE the in-box metadata emitter
//! writes (`tests/fixtures/assembly/MetadataEmitter`, shape
//! `manifest_cultures_and_forwarder`).

use crate::common;

use borzoi_assembly::{AssemblyIdentity, Ecma335Assembly, EcmaView, Version};

#[test]
fn culture_qualified_names_are_the_own_and_referenced_cultured_names() {
    let bytes = common::emit_metadata_fixture("manifest_cultures_and_forwarder");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let mut names = view
        .culture_qualified_names()
        .expect("the reader reads cultures");
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
