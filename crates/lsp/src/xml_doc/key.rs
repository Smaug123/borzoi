//! The documentation-comment ID of a referenced-assembly symbol, from the
//! project's [`AssemblyEnv`].
//!
//! The ID *format* is [`borzoi_assembly::doc_id`]'s, pinned against Roslyn's and
//! fsc's own `.xml` output. What this adds is the env side — walking a handle's
//! enclosing chain — and the two refusals that keep a lookup exact:
//!
//! - a handle the env cannot place under a top-level type has no knowable
//!   enclosing chain, so no knowable ID (a guessed `T:Name` could be another
//!   type's key);
//! - a target whose ID some *other* target of the same assembly also
//!   generates cannot be told apart from it by key, so whichever entry the
//!   file has is not provably this one's. The format is not injective:
//!   `T:A.B` names both `namespace A { type B }` and a type `B` nested in a
//!   type `A` (and `M:A.B.F` a member of either); two members can differ only
//!   in what the format does not encode (static vs instance). Rather than one
//!   guard per shape, [`KeyCensus`] counts every key every target of the
//!   assembly generates, and any key generated twice is refused for all its
//!   targets — which makes the map from target to committed key injective by
//!   construction.

use std::collections::HashSet;
use std::path::Path;

use borzoi_assembly::doc_id::{TypeDocName, member_doc_id, type_doc_name};
use borzoi_sema::{AssemblyEnv, EntityHandle, MemberIndex};

/// A referenced-assembly symbol whose documentation hover wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocTarget {
    Entity(EntityHandle),
    Member {
        parent: EntityHandle,
        idx: MemberIndex,
    },
}

impl DocTarget {
    /// The entity whose assembly the symbol belongs to.
    pub fn owner(self) -> EntityHandle {
        match self {
            DocTarget::Entity(handle) => handle,
            DocTarget::Member { parent, .. } => parent,
        }
    }
}

/// Why a symbol has no documentation-comment ID to look up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// The entity is not reachable from any top-level type in the env, so its
    /// enclosing chain — and hence its ID — is unknown.
    Unplaced,
    /// Another target of the same assembly generates the same ID.
    Shared,
}

/// The documentation-comment IDs that more than one target of one assembly
/// generates — every type and every member under the assembly's top-level
/// types, keyed exactly as [`doc_key`] keys them. Built in one pass; the
/// lookup layer caches one per assembly per env.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyCensus {
    shared: HashSet<String>,
}

impl KeyCensus {
    /// The census of the assembly read from `dll`: every target under the
    /// env's top-level types whose [`AssemblyEnv::assembly_path`] is `dll`.
    pub fn of_assembly(env: &AssemblyEnv, dll: &Path) -> KeyCensus {
        let mut seen: HashSet<String> = HashSet::new();
        let mut shared: HashSet<String> = HashSet::new();
        let mut note = |key: String| {
            if !seen.insert(key.clone()) {
                shared.insert(key);
            }
        };
        fn walk(
            env: &AssemblyEnv,
            handle: EntityHandle,
            enclosing: Option<&TypeDocName>,
            note: &mut impl FnMut(String),
        ) {
            let entity = env.entity(handle);
            let name = type_doc_name(entity, enclosing);
            note(name.type_id());
            for member in &entity.members {
                note(member_doc_id(&name, member));
            }
            for &child in env.children(handle) {
                walk(env, child, Some(&name), note);
            }
        }
        for &root in env.top_level_handles() {
            if env.assembly_path(root) == Some(dll) {
                walk(env, root, None, &mut note);
            }
        }
        KeyCensus { shared }
    }

    /// How many keys the assembly generates more than once.
    pub fn shared_keys(&self) -> usize {
        self.shared.len()
    }
}

/// The documentation-comment ID of `target`, refused when `census` (the
/// census of `target`'s assembly) records it as generated more than once.
pub fn doc_key(
    env: &AssemblyEnv,
    census: &KeyCensus,
    target: DocTarget,
) -> Result<String, KeyError> {
    let key = match target {
        DocTarget::Entity(handle) => type_name(env, handle)?.type_id(),
        DocTarget::Member { parent, idx } => {
            member_doc_id(&type_name(env, parent)?, env.member_at(parent, idx))
        }
    };
    if census.shared.contains(&key) {
        Err(KeyError::Shared)
    } else {
        Ok(key)
    }
}

/// The doc-ID type name of `handle`, threaded down its enclosing chain.
fn type_name(env: &AssemblyEnv, handle: EntityHandle) -> Result<TypeDocName, KeyError> {
    let chain = env
        .enclosing_chain_from_root(handle)
        .ok_or(KeyError::Unplaced)?;
    let mut name: Option<TypeDocName> = None;
    for link in chain {
        name = Some(type_doc_name(env.entity(link), name.as_ref()));
    }
    // A chain from a root is never empty: it ends at `handle`.
    name.ok_or(KeyError::Unplaced)
}
