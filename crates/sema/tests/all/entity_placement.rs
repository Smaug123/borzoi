//! Where an entity sits in an [`AssemblyEnv`] — its enclosing chain and the
//! source full name spelled from it — held to the depth-first search from the
//! top-level types that defines it
//! ([`borzoi_sema::test_support::entity_placement`]).
//!
//! The env answers both from links it records while it interns the forest, in
//! time linear in the entity's depth; the search costs time linear in the
//! whole env. They must agree on every entity: on generated forests, whose
//! names collide across roots and depths so an answer keyed by name rather than
//! by position shows, and on the real FSharp.Core and BCL reference assemblies.

use borzoi_assembly::{Access, Entity, EntityKind};
use borzoi_sema::test_support::entity_placement::{
    naive_enclosing_chain_from_root, naive_entity_full_name,
};
use borzoi_sema::{AssemblyEnv, EntityHandle};
use proptest::prelude::*;

/// A generated entity: which of a few colliding names it bears, whether that
/// name is a source name over a distinct compiled one, and its nested types.
#[derive(Debug, Clone)]
struct Node {
    name: u8,
    source_named: bool,
    children: Vec<Node>,
}

fn node() -> impl Strategy<Value = Node> {
    let leaf = (0u8..3, any::<bool>()).prop_map(|(name, source_named)| Node {
        name,
        source_named,
        children: vec![],
    });
    leaf.prop_recursive(4, 40, 3, |inner| {
        (0u8..3, any::<bool>(), prop::collection::vec(inner, 0..4)).prop_map(
            |(name, source_named, children)| Node {
                name,
                source_named,
                children,
            },
        )
    })
}

/// Top-level types, each in one of a few namespaces (the empty one included).
fn forest() -> impl Strategy<Value = Vec<(u8, Node)>> {
    prop::collection::vec((0u8..3, node()), 0..6)
}

fn namespace(index: u8) -> Vec<String> {
    match index {
        0 => vec![],
        1 => vec!["A".to_string()],
        _ => vec!["A".to_string(), "B".to_string()],
    }
}

/// An entity with nothing but the given placement, cut from a real one so the
/// test does not spell out every field of the assembly model.
fn entity(template: &Entity, namespace: Vec<String>, node: &Node) -> Entity {
    let mut e = template.clone();
    e.namespace = namespace;
    e.kind = EntityKind::Class;
    e.access = Access::Public;
    e.generic_parameters = vec![];
    e.name = format!("Name{}", node.name);
    e.source_name = node.source_named.then(|| format!("Source{}", node.name));
    e.nested_types = node
        .children
        .iter()
        .map(|c| entity(template, vec![], c))
        .collect();
    e
}

fn template() -> Entity {
    let env = crate::common::full_bcl_env();
    let mut e = env.entity(env.top_level_handles()[0]).clone();
    e.members.clear();
    e.nested_types.clear();
    e
}

fn assert_placement_matches_search(env: &AssemblyEnv, handle: EntityHandle) {
    assert_eq!(
        env.enclosing_chain_from_root(handle),
        naive_enclosing_chain_from_root(env, handle),
        "enclosing chain of {handle:?}"
    );
    assert_eq!(
        env.entity_full_name(handle),
        naive_entity_full_name(env, handle),
        "full name of {handle:?}"
    );
}

proptest! {
    #[test]
    fn placement_of_every_generated_entity_matches_the_search(roots in forest()) {
        let template = template();
        let env = AssemblyEnv::from_entities(
            roots
                .iter()
                .map(|(ns, root)| entity(&template, namespace(*ns), root))
                .collect(),
        );
        for handle in env.all_handles() {
            assert_placement_matches_search(&env, handle);
        }
    }
}

/// Every seventh entity of the real reference assemblies, nested types among
/// them: the search over all of them would cost time quadratic in the env.
#[test]
fn placement_of_reference_assembly_entities_matches_the_search() {
    let env = crate::common::full_bcl_env();
    let mut nested = 0;
    for handle in env.all_handles().step_by(7) {
        assert_placement_matches_search(env, handle);
        if env
            .enclosing_chain_from_root(handle)
            .is_some_and(|c| c.len() > 1)
        {
            nested += 1;
        }
    }
    // The sample reaches nested types, or it would not test the descent.
    assert!(nested > 100, "only {nested} nested entities sampled");
}
