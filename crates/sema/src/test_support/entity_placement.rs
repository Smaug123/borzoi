//! Reference implementations of where an entity sits in an [`AssemblyEnv`]:
//! its chain of enclosing entities and the source full name spelled from it.
//!
//! Each one searches the whole forest from the top-level types, depth first,
//! taking the first path that reaches the entity. That is the definition the
//! env's own answers ([`AssemblyEnv::enclosing_chain_from_root`],
//! [`AssemblyEnv::entity_full_name`]) are held to, by
//! `tests/all/entity_placement.rs`. The search costs time linear in the size of
//! the whole env per question, so nothing at runtime may ask it.

use crate::{AssemblyEnv, EntityHandle};

/// The chain of entities from a top-level type down to `target`, `target`
/// last: the first one a depth-first search from each top-level type in turn
/// reaches it by. `None` when no top-level type reaches it.
pub fn naive_enclosing_chain_from_root(
    env: &AssemblyEnv,
    target: EntityHandle,
) -> Option<Vec<EntityHandle>> {
    fn search(
        env: &AssemblyEnv,
        current: EntityHandle,
        target: EntityHandle,
        chain: &mut Vec<EntityHandle>,
    ) -> bool {
        chain.push(current);
        if current == target {
            return true;
        }
        for &child in env.children(current) {
            if search(env, child, target, chain) {
                return true;
            }
        }
        chain.pop();
        false
    }
    let mut chain = Vec::new();
    env.top_level_handles()
        .iter()
        .any(|&root| search(env, root, target, &mut chain))
        .then_some(chain)
}

/// The F# source full name of `target`: the namespace of the top-level type
/// [`naive_enclosing_chain_from_root`] reaches it from, then each entity's
/// source name down the chain (`List`, not `ListModule`), dot-joined. An
/// entity no top-level type reaches is named by its own namespace and source
/// name.
pub fn naive_entity_full_name(env: &AssemblyEnv, target: EntityHandle) -> String {
    let source_name = |h: EntityHandle| {
        let e = env.entity(h);
        e.source_name.clone().unwrap_or_else(|| e.name.clone())
    };
    let mut segments: Vec<String> = Vec::new();
    match naive_enclosing_chain_from_root(env, target) {
        Some(chain) => {
            segments.extend(env.entity(chain[0]).namespace.iter().cloned());
            segments.extend(chain.into_iter().map(source_name));
        }
        None => {
            segments.extend(env.entity(target).namespace.iter().cloned());
            segments.push(source_name(target));
        }
    }
    segments.join(".")
}
