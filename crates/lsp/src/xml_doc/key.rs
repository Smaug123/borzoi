//! The documentation-comment ID of a referenced-assembly symbol, from the
//! project's [`AssemblyEnv`].
//!
//! The ID *format* is [`borzoi_assembly::doc_id`]'s, pinned against Roslyn's and
//! fsc's own `.xml` output. What this adds is the env side — walking a handle's
//! enclosing chain — and the refusals that keep a lookup exact:
//!
//! - a *member* of an assembly the F# compiler may have written
//!   ([`DocIdDialect::FSharp`]) has no knowable ID: fsc keys a member from its
//!   F# signature, which IL does not record (a measure type parameter counts
//!   towards the generic arity, then is erased), so the ID computed from IL can
//!   be the ID fsc wrote for a *different* member — and no census of computed
//!   IDs sees that, since the colliding ID is never computed for its owner. A
//!   member of an assembly whose dialect the env does not record is refused
//!   the same way. A *type*'s ID is its compiled name in both dialects;
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

use std::collections::{HashMap, HashSet};
use std::path::Path;

use borzoi_assembly::Member;
use borzoi_assembly::doc_id::{
    DocIdDialect, TypeDocName, member_doc_id, method_id_head, type_doc_name,
};
use borzoi_sema::{AssemblyEnv, EntityHandle, MemberIndex};

/// A referenced-assembly symbol whose documentation hover wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// A member of an assembly the F# compiler may have written: the doc file
    /// keys it from its F# signature, which the IL-derived ID need not match.
    FSharpMember,
    /// A member of an assembly whose [`DocIdDialect`] the env does not record.
    DialectUnknown,
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

/// Every target of an env by documentation-comment ID, across all its
/// assemblies — the inverse of [`doc_key`], for following an `<inheritdoc
/// cref="…">`, which names its target by ID alone. Keys are generated exactly
/// as [`doc_key`] generates them; a key with several targets (in one assembly
/// or across several) keeps them all, so a caller sees the ambiguity.
///
/// A property's or event's accessors are methods with IDs of their own
/// (`M:N.T.get_P`), which the model folds into their owner and so has no
/// target for; the index keeps their ID heads apart, so that an ID one could
/// carry is never taken as naming only the other targets it matches.
#[derive(Debug, Clone, Default)]
pub struct DocIdIndex {
    by_key: HashMap<String, Vec<DocTarget>>,
    /// [`method_id_head`] of every accessor.
    accessor_heads: HashSet<String>,
    /// The `T:` IDs of the types some accessor of which has no recorded
    /// name, so that any method ID of the type may be an accessor's.
    unnamed_accessor_types: HashSet<String>,
    /// The namespaces (dotted) of the top-level types read in F#'s source
    /// view, or for one in the global namespace its name (with its arity
    /// suffix, and without): that view does not show every metadata type
    /// beneath them.
    fsharp_prefixes: HashSet<String>,
    /// Whether some indexed type is a No-PIA local type
    /// ([`borzoi_assembly::Entity::is_no_pia_local`]), which Roslyn's
    /// importer hides from name lookup.
    has_no_pia_local_type: bool,
}

impl DocIdIndex {
    /// The index of every target under every top-level type of `env`.
    pub fn of_env(env: &AssemblyEnv) -> DocIdIndex {
        Self::of_roots(env, |_| true)
    }

    /// The index of the targets of one assembly of `env`: those under the
    /// top-level types read from `dll`.
    pub fn of_assembly(env: &AssemblyEnv, dll: &Path) -> DocIdIndex {
        Self::of_roots(env, |root| env.assembly_path(root) == Some(dll))
    }

    fn of_roots(env: &AssemblyEnv, include: impl Fn(EntityHandle) -> bool) -> DocIdIndex {
        fn walk(
            env: &AssemblyEnv,
            handle: EntityHandle,
            enclosing: Option<&TypeDocName>,
            index: &mut DocIdIndex,
        ) {
            let entity = env.entity(handle);
            index.has_no_pia_local_type |= entity.is_no_pia_local;
            let name = type_doc_name(entity, enclosing);
            index
                .by_key
                .entry(name.type_id())
                .or_default()
                .push(DocTarget::Entity(handle));
            for idx in env.member_indices(handle) {
                let member = env.member_at(handle, idx);
                index
                    .by_key
                    .entry(member_doc_id(&name, member))
                    .or_default()
                    .push(DocTarget::Member {
                        parent: handle,
                        idx,
                    });
                let (slots, accessors) = match member {
                    Member::Property(p) => (
                        &p.accessor_slots,
                        usize::from(p.has_getter) + usize::from(p.has_setter),
                    ),
                    // An event has `add` and `remove`, and perhaps `fire`.
                    Member::Event(e) => (&e.accessor_slots, 2),
                    Member::Method(_) | Member::Field(_) => continue,
                };
                if slots.len() < accessors || slots.iter().any(|s| s.name.is_empty()) {
                    index.unnamed_accessor_types.insert(name.type_id());
                }
                for slot in slots.iter().filter(|s| !s.name.is_empty()) {
                    index
                        .accessor_heads
                        .insert(method_id_head(&name, &slot.name));
                }
            }
            for &child in env.children(handle) {
                walk(env, child, Some(&name), index);
            }
        }
        let mut index = DocIdIndex::default();
        for &root in env.top_level_handles() {
            if include(root) {
                walk(env, root, None, &mut index);
                if env.has_authoritative_fsharp_signature(root) {
                    let entity = env.entity(root);
                    if entity.namespace.is_empty() {
                        // As its ID spells it (`` G`1 ``), and as bare.
                        index
                            .fsharp_prefixes
                            .insert(type_doc_name(entity, None).full().to_string());
                        index.fsharp_prefixes.insert(entity.name.clone());
                    } else {
                        index.fsharp_prefixes.insert(entity.namespace.join("."));
                    }
                }
            }
        }
        index
    }

    /// Whether `key` may be the ID of some property's or event's accessor:
    /// a method ID whose head an accessor carries, or any method ID of a type
    /// with an accessor whose name was not recorded.
    pub fn may_name_an_accessor(&self, key: &str) -> bool {
        let Some(rest) = key.strip_prefix("M:") else {
            return false;
        };
        // The declaring type is a generic definition, whose name may carry
        // `` `n `` but never `(` or `~`; those start the parameters and a
        // conversion's return type. A method's own arity is the `` ``n ``
        // ending what remains.
        let head = &key[..2 + rest.find(['(', '~']).unwrap_or(rest.len())];
        let head = match head.rfind("``") {
            Some(at) if head[at + 2..].bytes().all(|b| b.is_ascii_digit()) => &head[..at],
            _ => head,
        };
        if self.accessor_heads.contains(head) {
            return true;
        }
        // Member names escape `.` to `#`, so the last `.` ends the type.
        head.rfind('.').is_some_and(|dot| {
            self.unnamed_accessor_types
                .contains(&format!("T:{}", &head[2..dot]))
        })
    }

    /// Whether some proper dotted prefix of `key`'s path (after its `X:`, up
    /// to any parameters) is a namespace a type read in F#'s source view
    /// occupies — or, for one in the global namespace, that type's name.
    pub fn under_an_fsharp_prefix(&self, key: &str) -> bool {
        let Some((_, rest)) = key.split_once(':') else {
            return false;
        };
        let path = rest.split(['(', '~']).next().unwrap_or(rest);
        path.match_indices('.')
            .any(|(dot, _)| self.fsharp_prefixes.contains(&path[..dot]))
    }

    /// Whether some indexed type is a No-PIA local type.
    pub fn has_no_pia_local_type(&self) -> bool {
        self.has_no_pia_local_type
    }

    /// Every target whose ID is `key`.
    pub fn targets(&self, key: &str) -> &[DocTarget] {
        self.by_key.get(key).map_or(&[], Vec::as_slice)
    }
}

/// The documentation-comment ID of `target`, refused when `census` (the
/// census of `target`'s assembly) records it as generated more than once, and
/// for a member unless its assembly's doc file is keyed in the
/// [`DocIdDialect::Metadata`] dialect.
pub fn doc_key(
    env: &AssemblyEnv,
    census: &KeyCensus,
    target: DocTarget,
) -> Result<String, KeyError> {
    let key = match target {
        DocTarget::Entity(handle) => type_name(env, handle)?.type_id(),
        DocTarget::Member { parent, idx } => match env.doc_id_dialect(parent) {
            Some(DocIdDialect::Metadata) => {
                member_doc_id(&type_name(env, parent)?, env.member_at(parent, idx))
            }
            Some(DocIdDialect::FSharp) => return Err(KeyError::FSharpMember),
            None => return Err(KeyError::DialectUnknown),
        },
    };
    if census.shared.contains(&key) {
        Err(KeyError::Shared)
    } else {
        Ok(key)
    }
}

/// The doc-ID type name of `handle`, threaded down its enclosing chain.
pub fn type_name(env: &AssemblyEnv, handle: EntityHandle) -> Result<TypeDocName, KeyError> {
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
