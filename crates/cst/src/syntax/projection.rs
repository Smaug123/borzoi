//! Versioned typed-AST **gate** — the language-version surface check
//! ([`docs/ast-versioning-plan.md`] D3/P2), the runtime-grounded companion to the
//! generated per-version facades (`syntax::v4_6` … `syntax::preview`).
//!
//! The `vN` *facades* (each owning every node type and enum of its version's
//! surface, e.g. `v8::Type`, the union [`Type`](super::Type) less the F# 9.0
//! `WithNull`) are **generated** by `tools/astgen` from the feature table
//! (`crates/cst/language-features.tsv`) and live in `super::generated`. What
//! stays here is the *gate*: given a parsed tree and a pinned
//! [`LanguageVersion`], find a node outside that surface. This is the
//! executable statement of "the `vN` projection is not total here" — the same
//! fact a caller turns into a diagnostic or a `.syntax()` drop (the D7
//! "incomplete, never wrong" floor). It reads [`kind_in_surface`], which reads
//! the same table rows, so gate and facade agree by construction.
//!
//! The kinds with an interval are the live `Kind` rows of the table:
//! `WHILE_BANG_EXPR` and `INTERSECTION_TYPE` (F# 8.0) and [`WITH_NULL_TYPE`]
//! (F# 9.0, FCS `LanguageFeature.NullnessChecking`); see
//! [`docs/completed/ast-versioning-nullness-proof.md`] for the mechanism proof
//! this productionises.
//!
//! [`docs/ast-versioning-plan.md`]: ../../../../docs/ast-versioning-plan.md
//! [`docs/completed/ast-versioning-nullness-proof.md`]: ../../../../docs/completed/ast-versioning-nullness-proof.md
//! [`WITH_NULL_TYPE`]: super::SyntaxKind::WITH_NULL_TYPE

use crate::language_version::LanguageVersion;
#[cfg(doc)]
use crate::syntax::DEFERRED_KIND_FEATURES;
use crate::syntax::{AstNode, SyntaxKind, SyntaxNode, Type as UnionType, kind_in_surface};

/// Whether a `Type` node of `kind` is legal at `lang`. A thin, `Type`-scoped
/// alias over the shared interval table ([`kind_in_surface`]) — kept because the
/// projection and its property suite phrase things in terms of the `Type`
/// surface. For non-`Type` kinds the answer is whatever the table says (vacuously
/// `true` for everything not yet gated).
pub fn type_kind_in_surface(kind: SyntaxKind, lang: LanguageVersion) -> bool {
    kind_in_surface(kind, lang)
}

/// The first node in `root`'s subtree (preorder) whose kind is outside `lang`'s
/// language surface, or `None` if the whole tree is viewable at `lang`.
///
/// The **general** language-version gate, over every modelled kind via the
/// shared [`kind_in_surface`] table — the generalisation of
/// [`first_out_of_surface_type`] from the `Type` enum to all of them. A `Some`
/// is simultaneously the node to **diagnose** (out of surface under the pin) and
/// the executable statement that the **`vN` projection is not total here**
/// (`docs/ast-versioning-plan.md` P2); a caller turns it into a diagnostic
/// and/or drops to [`SyntaxNode`] for that node (the D7 "incomplete, never
/// wrong" floor).
///
/// It gates against [`kind_in_surface`], which knows only the feature table's
/// live `Kind` rows. The deferred ones ([`DEFERRED_KIND_FEATURES`]: interpolated
/// strings at 5.0, `_.M` at 8.0, from-end indexing at preview) are in surface at
/// every version until they go live, so below their version this
/// **under-reports** them (`docs/ast-versioning-plan.md`, "The feature table").
/// Below 8.0 it also finds `WHILE_BANG_EXPR`, which is not a `Type`, so there it
/// can find a node [`first_out_of_surface_type`] does not.
pub fn first_out_of_surface(root: &SyntaxNode, lang: LanguageVersion) -> Option<SyntaxNode> {
    root.descendants()
        .find(|n| !kind_in_surface(n.kind(), lang))
}

/// The first `Type` node in `root`'s subtree (preorder) that is outside `lang`'s
/// type surface, or `None` if every `Type` node is viewable at `lang`. The
/// `Type`-restricted form of [`first_out_of_surface`]; see it for the
/// gate-≡-totality framing.
pub fn first_out_of_surface_type(root: &SyntaxNode, lang: LanguageVersion) -> Option<SyntaxNode> {
    root.descendants()
        .find(|n| UnionType::can_cast(n.kind()) && !type_kind_in_surface(n.kind(), lang))
}
