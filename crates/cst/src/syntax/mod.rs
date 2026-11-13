//! Concrete-syntax tree.
//!
//! The parser writes a lossless tree as one preorder table per file (`flat`,
//! see `docs/flat-syntax-tree-plan.md`); [`SyntaxNode`] and [`SyntaxToken`]
//! are handles into it. That *substrate* — the handles, [`SyntaxKind`],
//! [`AstNode`] and the [`projection`] gate — is what this module always
//! exports.
//!
//! The *typed* facade (`ImplFile`, `ModuleOrNamespace`, the `Expr` / `Pat` /
//! `Type` / `ModuleDecl` dispatch enums, …) wraps those nodes with
//! strongly-typed accessors so consumers can navigate the tree without matching
//! on raw [`SyntaxKind`]s. It is the *union* over every language version and
//! gains a variant whenever a construct is modelled, so it is exported, at
//! these paths, only under the `unstable-union` feature
//! (`docs/ast-versioning-plan.md` D2); so are the `v8` / `v9` facades, which
//! re-export its node types.

#[cfg(feature = "accessor-trace")]
pub mod accessor_trace;
mod kinds;
pub mod projection;

/// The first statement of every public accessor in `union`: `accessor!("Type::name")`.
/// It expands to nothing unless the `accessor-trace` feature is on, when it
/// opens an `accessor_trace` guard so a test can see which accessors a
/// computation reads. The parser differential's coverage check depends on every
/// accessor carrying it (`tests/all/accessor_coverage.rs` checks that).
macro_rules! accessor {
    ($name:literal) => {
        #[cfg(feature = "accessor-trace")]
        let _accessor_guard = crate::syntax::accessor_trace::enter($name);
    };
}

// The borzoi-astgen-generated half of the typed facade: the dispatch enums and
// their member newtypes (re-exported through `union`) and the per-version
// facades. See `generated/mod.rs`. Without `unstable-union` nothing outside the
// crate can reach it.
#[cfg_attr(not(feature = "unstable-union"), allow(dead_code))]
mod generated;

// The feature table's generated half (`crates/cst/language-features.tsv`):
// `LanguageFeature`, re-exported from `crate::language_version`, and the
// kinds it introduced.
pub(crate) use generated::features;
pub use generated::features::{DEFERRED_KIND_FEATURES, kind_feature};

// The frozen per-version facades (plan PR E): `borzoi_cst::syntax::v8` /
// `::v9`. The names re-exported from `union` (no `vN::` prefix) are the *union*
// surface — the maximal, every-version superset; pin a `vN` for a frozen,
// exhaustive view (`docs/ast-versioning-plan.md` D4). They re-export the
// union's node types and accessors, so they are behind its feature.
#[cfg(feature = "unstable-union")]
pub use generated::{v8, v9};
pub use kinds::{KindInterval, SyntaxKind, kind_in_surface, kind_interval};

/// Ties [`SyntaxKind`] to rowan's `u16`-tagged trees, for the rowan reference
/// differential (`tests/all/flat_tree_reference.rs`) only. Zero-sized.
#[cfg(feature = "rowan-reference")]
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FSharpLang {}

#[cfg(feature = "rowan-reference")]
impl rowan::Language for FSharpLang {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        SyntaxKind::from_raw(raw.0).expect("rowan SyntaxKind out of range")
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

mod flat;
pub(crate) use flat::{Builder, Checkpoint};
pub use flat::{
    Direction, NodeOrToken, Structure, SyntaxElement, SyntaxNode, SyntaxText, SyntaxToken,
    TextRange, TextSize, TokenAtOffset, WalkEvent,
};

/// Common interface for the typed-AST facade. One implementor per node-kind
/// (`ImplFile`, `ModuleOrNamespace`, …). Mirrors rust-analyzer's `AstNode`.
///
/// `cast` succeeds exactly when `can_cast` accepts the node's kind (astgen
/// emits both from one kind list). [`SyntaxNode::descendants_of`] relies on
/// it, testing only the kind before it builds a handle.
pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

// The union typed facade (`union.rs`). Its items are `crate::syntax::Expr` and
// so on either way; the feature decides only whether that path is public.
// Without it, the crate's unit tests and `projection` are its only readers.
#[cfg_attr(not(feature = "unstable-union"), allow(dead_code))]
mod union;
#[cfg(feature = "unstable-union")]
pub use union::*;
#[cfg(not(feature = "unstable-union"))]
pub(crate) use union::*;
