//! The type table: every [`Ty`] is an id into one, and each distinct type is
//! held once (`docs/sema-interning-plan.md`, D4 and Stage 8).
//!
//! A table interns a node — its [`TyKind`], whose children are ids of the same
//! table — and hands back the id it already holds for an equal node, so two
//! types are equal exactly when their ids are, and a type is `Copy`. Each node
//! also records its tree's size and whether a [`TyKind::Var`] occurs in it,
//! computed once from its children, which makes [`TyTable::size`] and
//! [`TyTable::is_ground`] constant-time.
//!
//! A shared subterm is one node however many times the tree holds it, so a
//! node's size counts the tree, not the table: `(t, t)` is `1 + 2·|t|` nodes
//! of tree over `|t| + 1` nodes of table. The type budget
//! ([`MAX_TYPE_NODES`](super::MAX_TYPE_NODES)) bounds trees, since every walk
//! that renders, resolves or unifies a type visits its tree.
//!
//! There are two kinds of table, and ids never cross between them:
//!
//! - An inference session's ([`TyTable::session`]): the terms one file's
//!   inference builds, inference variables included. It lives as long as the
//!   session.
//! - The env's ([`TyTable::ground`], [`AssemblyEnv::types`]): the types
//!   inference publishes, and the schemes a file declares, all ground (no
//!   inference variable; a [`TyKind::Param`] is ground). It only grows, like
//!   the env's name table, and the LSP bounds the two together
//!   ([`AssemblyEnv::interned_count`]). Interning a variable into it panics.
//!
//! A session reads an env type by copying it in ([`TyTable::import`], or an
//! instantiation), and publishes a type by copying it out. In a debug build
//! each id carries its table's tag, and comparing ids of two tables, or
//! reading one against the wrong table, panics.
//!
//! Ids are handed out in interning order, so like the name table's they never
//! order anything: [`Ty`] has no `Ord`.
//!
//! Under `interning-reference` in a debug build the table keeps, beside its
//! index, a reference index keyed by each node's spelling — its paths' carried
//! segments and its children's ids — and asserts on every intern that the two
//! agree, so a node is interned once exactly when its spelling is.
//!
//! [`AssemblyEnv::types`]: crate::AssemblyEnv::types
//! [`AssemblyEnv::interned_count`]: crate::AssemblyEnv::interned_count

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{PoisonError, RwLock};

use super::{Ty, TyKind, TyPath, TySize, TyVid};
use crate::hash::HashMap;
use crate::names::{AppendOnly, NameTable};

/// What a table may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holds {
    /// Ground types only: the env's table.
    Ground,
    /// Any term, inference variables included: a session's.
    Terms,
}

/// One interned type: its node, and what is computed once from its children.
#[derive(Debug)]
struct Node {
    kind: TyKind,
    /// The tree's node count (saturating) and depth.
    size: TySize,
    /// Whether a [`TyKind::Var`] occurs in the tree.
    has_var: bool,
}

/// The probe that needs hashing: node → id.
#[derive(Default)]
struct Index {
    ids: HashMap<TyKind, u32>,
    /// How many nodes have been interned: the next node's id.
    count: u32,
    /// Each node's spelling → its id: the reference [`Self::ids`] is held to.
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    reference: HashMap<String, u32>,
}

/// A table of interned types. See the module docs.
pub(crate) struct TyTable {
    /// This table's tag, unique in the process: what a debug build's ids
    /// carry.
    tag: u32,
    holds: Holds,
    nodes: AppendOnly<Node>,
    /// Each inference variable's node, by the variable's index: the one
    /// node a walk interns most often, read here without hashing.
    vars: AppendOnly<Ty>,
    index: RwLock<Index>,
}

impl std::fmt::Debug for TyTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TyTable")
            .field("tag", &self.tag)
            .field("holds", &self.holds)
            .field("nodes", &self.read().count)
            .finish()
    }
}

/// The default table is an env's: ground types only, as the env's name table
/// is the default [`NameTable`].
impl Default for TyTable {
    fn default() -> Self {
        TyTable::ground()
    }
}

impl TyTable {
    fn new(holds: Holds) -> TyTable {
        static NEXT_TAG: AtomicU32 = AtomicU32::new(0);
        TyTable {
            tag: NEXT_TAG.fetch_add(1, Ordering::Relaxed),
            holds,
            nodes: AppendOnly::default(),
            vars: AppendOnly::default(),
            index: RwLock::new(Index::default()),
        }
    }

    /// An env's table: ground types only.
    pub(crate) fn ground() -> TyTable {
        TyTable::new(Holds::Ground)
    }

    /// An inference session's table: any term.
    pub(crate) fn session() -> TyTable {
        TyTable::new(Holds::Terms)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Index> {
        // Nothing panics while the lock is held, so a poisoned lock still
        // guards a consistent index.
        self.index.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Index> {
        self.index.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn id(&self, index: u32) -> Ty {
        Ty {
            index,
            #[cfg(debug_assertions)]
            table: self.tag,
        }
    }

    /// Assert, in a debug build, that `ty` is this table's.
    fn check(&self, ty: Ty) {
        #[cfg(debug_assertions)]
        assert_eq!(ty.table, self.tag, "a Ty read against another type table");
        #[cfg(not(debug_assertions))]
        let _ = ty;
    }

    fn node(&self, ty: Ty) -> &Node {
        self.check(ty);
        self.nodes
            .get(ty.index)
            .expect("a Ty's node is written before the Ty is issued")
    }

    /// The node `kind`, interned if it is new. Its children must be this
    /// table's.
    ///
    /// # Panics
    ///
    /// Panics when `kind` is a [`TyKind::Var`] and this is an env's table,
    /// which holds ground types only.
    pub(crate) fn intern(&self, kind: TyKind) -> Ty {
        // An env's table is shared, so it is probed under the read lock
        // first; a session's has one writer, and a session interns mostly
        // new nodes, so it is probed once, under the write lock, below.
        if self.holds == Holds::Ground {
            let found = self.read().ids.get(&kind).copied();
            if let Some(index) = found {
                self.check_reference(&kind, Some(index));
                return self.id(index);
            }
        }
        let node = self.node_of(kind);
        let mut index = self.write();
        // The only probe of a session's table, and for an env's, another
        // thread may have interned the node between the two locks.
        if let Some(&existing) = index.ids.get(&node.kind) {
            drop(index);
            self.check_reference(&node.kind, Some(existing));
            return self.id(existing);
        }
        let new = index.count;
        index.count = new.checked_add(1).expect("fewer than 2^32 types");
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        {
            let spelled = spelling(&node.kind);
            assert_eq!(
                index.reference.get(&spelled),
                None,
                "a type the table holds was interned again: {spelled}"
            );
            index.reference.insert(spelled, new);
        }
        index.ids.insert(node.kind.clone(), new);
        self.nodes.set(new, node);
        self.id(new)
    }

    /// The node `kind`, with what its children give it.
    fn node_of(&self, kind: TyKind) -> Node {
        let mut size = TySize { nodes: 1, depth: 1 };
        let mut has_var = false;
        let mut child = |c: Ty| {
            let n = self.node(c);
            size.nodes = size.nodes.saturating_add(n.size.nodes);
            size.depth = size.depth.max(n.size.depth.saturating_add(1));
            has_var |= n.has_var;
        };
        match &kind {
            TyKind::Named { args, .. } | TyKind::Project { args, .. } | TyKind::Tuple(args) => {
                args.iter().copied().for_each(&mut child);
            }
            TyKind::Array { elem, .. } => child(*elem),
            TyKind::Fun { arg, ret } => {
                child(*arg);
                child(*ret);
            }
            TyKind::Param(_) => {}
            TyKind::Var(_) => {
                assert!(
                    self.holds == Holds::Terms,
                    "an inference variable interned into an env's type table, \
                     which holds ground types only"
                );
                has_var = true;
            }
        }
        Node {
            kind,
            size,
            has_var,
        }
    }

    /// Assert, under `interning-reference` in a debug build, that the
    /// reference index holds `kind`'s spelling at `found`, and that the node
    /// at `found` spells it.
    fn check_reference(&self, kind: &TyKind, found: Option<u32>) {
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        {
            let spelled = spelling(kind);
            assert_eq!(
                self.read().reference.get(&spelled).copied(),
                found,
                "the type table and its reference disagree on {spelled}"
            );
            if let Some(found) = found {
                assert_eq!(
                    spelling(&self.node(self.id(found)).kind),
                    spelled,
                    "a type's id holds another node"
                );
            }
        }
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = (kind, found);
    }

    /// `ty`'s node.
    pub(crate) fn kind(&self, ty: Ty) -> &TyKind {
        &self.node(ty).kind
    }

    /// `ty`'s node count and depth, as the type budget counts them
    /// ([`MAX_TYPE_NODES`](super::MAX_TYPE_NODES),
    /// [`MAX_TYPE_DEPTH`](super::MAX_TYPE_DEPTH)): of its tree, a shared
    /// subterm counted at each place it occurs, the node count saturating.
    pub(crate) fn size(&self, ty: Ty) -> TySize {
        self.node(ty).size
    }

    /// Whether `ty` is fully resolved: no [`TyKind::Var`] occurs in it. The
    /// boundary check inference applies before publishing a type, so an
    /// unsolved variable becomes silence (D5) rather than a wrong answer.
    pub(crate) fn is_ground(&self, ty: Ty) -> bool {
        !self.node(ty).has_var
    }

    /// How many types the table holds.
    pub(crate) fn len(&self) -> usize {
        self.read().count as usize
    }

    /// Every type the table holds, in the order they were interned.
    #[cfg(feature = "test-support")]
    pub(crate) fn all(&self) -> impl Iterator<Item = Ty> + '_ {
        let count = self.read().count;
        (0..count).map(|index| self.id(index))
    }

    /// A nullary [`TyKind::Named`] from a dotted canonical path
    /// (`"System.Int32"`), its path interned in `names`.
    pub(crate) fn named(&self, names: &NameTable, dotted: &str) -> Ty {
        self.app(TyPath::dotted(names, dotted), Vec::new())
    }

    /// A nullary [`TyKind::Named`] from an already-split canonical path.
    pub(crate) fn named_path<S: AsRef<str>>(&self, names: &NameTable, path: &[S]) -> Ty {
        self.app(TyPath::new(names, path), Vec::new())
    }

    /// The named type `path` applied to `args`.
    pub(crate) fn app(&self, path: TyPath, args: impl Into<Box<[Ty]>>) -> Ty {
        self.intern(TyKind::Named {
            path,
            args: args.into(),
        })
    }

    /// The project type `path` applied to `args`.
    pub(crate) fn project(&self, path: TyPath, args: impl Into<Box<[Ty]>>) -> Ty {
        self.intern(TyKind::Project {
            path,
            args: args.into(),
        })
    }

    /// The array of `elem` of rank `rank`.
    pub(crate) fn array(&self, elem: Ty, rank: u32) -> Ty {
        self.intern(TyKind::Array { elem, rank })
    }

    /// The tuple of `elems`.
    pub(crate) fn tuple(&self, elems: impl Into<Box<[Ty]>>) -> Ty {
        self.intern(TyKind::Tuple(elems.into()))
    }

    /// The function `arg -> ret`.
    pub(crate) fn fun(&self, arg: Ty, ret: Ty) -> Ty {
        self.intern(TyKind::Fun { arg, ret })
    }

    /// The quantified parameter `i`.
    pub(crate) fn param(&self, i: u32) -> Ty {
        self.intern(TyKind::Param(i))
    }

    /// The inference variable `v`. A session's table only.
    pub(crate) fn var(&self, v: TyVid) -> Ty {
        let ty = *self
            .vars
            .get_or_init(v.index(), || self.intern(TyKind::Var(v)));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        self.check_reference(&TyKind::Var(v), Some(ty.index));
        ty
    }

    /// Whether `ty` is the nullary [`TyKind::Named`] at `path`.
    pub(crate) fn is_named(&self, names: &NameTable, ty: Ty, path: &[&str]) -> bool {
        matches!(self.kind(ty), TyKind::Named { path: p, args } if args.is_empty() && p.is(names, path))
    }

    /// `ty`, a type of `from`, as a type of this table: the same tree, each
    /// node interned here. `memo` maps `from`'s ids already copied, and is
    /// filled with the ones this copies.
    ///
    /// # Panics
    ///
    /// Panics when `ty` holds an inference variable and this is an env's
    /// table.
    pub(crate) fn import(&self, from: &TyTable, ty: Ty, memo: &mut HashMap<Ty, Ty>) -> Ty {
        if let Some(&done) = memo.get(&ty) {
            return done;
        }
        let kind = match from.kind(ty) {
            TyKind::Named { path, args } => TyKind::Named {
                path: path.clone(),
                args: self.import_all(from, args, memo),
            },
            TyKind::Project { path, args } => TyKind::Project {
                path: path.clone(),
                args: self.import_all(from, args, memo),
            },
            TyKind::Array { elem, rank } => TyKind::Array {
                elem: self.import(from, *elem, memo),
                rank: *rank,
            },
            TyKind::Tuple(elems) => TyKind::Tuple(self.import_all(from, elems, memo)),
            TyKind::Fun { arg, ret } => TyKind::Fun {
                arg: self.import(from, *arg, memo),
                ret: self.import(from, *ret, memo),
            },
            TyKind::Param(i) => TyKind::Param(*i),
            TyKind::Var(v) => TyKind::Var(*v),
        };
        let copied = self.intern(kind);
        memo.insert(ty, copied);
        copied
    }

    fn import_all(&self, from: &TyTable, args: &[Ty], memo: &mut HashMap<Ty, Ty>) -> Box<[Ty]> {
        args.iter().map(|&a| self.import(from, a, memo)).collect()
    }

    /// `ty`, a type of `from` whose paths are ids of `from_names`, as a type
    /// of this table whose paths are ids of `to_names`.
    #[cfg(feature = "test-support")]
    pub(crate) fn rekey(
        &self,
        from: &TyTable,
        from_names: &NameTable,
        to_names: &NameTable,
        ty: Ty,
    ) -> Ty {
        let all = |args: &[Ty]| -> Box<[Ty]> {
            args.iter()
                .map(|&a| self.rekey(from, from_names, to_names, a))
                .collect()
        };
        let kind = match from.kind(ty) {
            TyKind::Named { path, args } => TyKind::Named {
                path: path.rekey(from_names, to_names),
                args: all(args),
            },
            TyKind::Project { path, args } => TyKind::Project {
                path: path.rekey(from_names, to_names),
                args: all(args),
            },
            TyKind::Array { elem, rank } => TyKind::Array {
                elem: self.rekey(from, from_names, to_names, *elem),
                rank: *rank,
            },
            TyKind::Tuple(elems) => TyKind::Tuple(all(elems)),
            TyKind::Fun { arg, ret } => TyKind::Fun {
                arg: self.rekey(from, from_names, to_names, *arg),
                ret: self.rekey(from, from_names, to_names, *ret),
            },
            TyKind::Param(i) => TyKind::Param(*i),
            TyKind::Var(v) => TyKind::Var(*v),
        };
        self.intern(kind)
    }
}

/// A node's spelling, for the reference index: its paths' carried segments
/// and its children's ids, built apart from [`TyKind`]'s own `Hash` and `Eq`.
#[cfg(all(feature = "interning-reference", debug_assertions))]
fn spelling(kind: &TyKind) -> String {
    let ids = |args: &[Ty]| {
        args.iter()
            .map(|a| a.index.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    match kind {
        TyKind::Named { path, args } => format!("named {:?} [{}]", path.text(), ids(args)),
        TyKind::Project { path, args } => format!("project {:?} [{}]", path.text(), ids(args)),
        TyKind::Array { elem, rank } => format!("array {} {rank}", elem.index),
        TyKind::Tuple(elems) => format!("tuple [{}]", ids(elems)),
        TyKind::Fun { arg, ret } => format!("fun {} {}", arg.index, ret.index),
        TyKind::Param(i) => format!("param {i}"),
        TyKind::Var(v) => format!("var {}", v.index()),
    }
}
