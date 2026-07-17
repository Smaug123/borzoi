//! Versioned typed-AST **gate** — the language-version surface check
//! ([`docs/ast-versioning-plan.md`] D3/P2), the runtime-grounded companion to the
//! generated per-version facades ([`v8`](super::v8) / [`v9`](super::v9)).
//!
//! The frozen `vN` *facades* (the distinct, exhaustive dispatch enums, e.g.
//! `v8::Type` = the union [`Type`](super::Type) minus the F# 9.0 `WithNull`) are
//! now **generated** by `tools/astgen` from the interval table and live in
//! `super::generated`. What stays here is the *gate*: given a parsed tree and a
//! pinned [`LanguageVersion`], find a node outside that surface. This is the
//! executable statement of "the `vN` projection is not total here" — the same
//! fact a caller turns into a diagnostic or a `.syntax()` drop (the D7
//! "incomplete, never wrong" floor). It reads the shared
//! [`kind_in_surface`] interval table, so gate and facade agree by construction.
//!
//! The single typed delta today is **nullness** ([`WITH_NULL_TYPE`], F# 9.0, FCS
//! `LanguageFeature.NullnessChecking`); see
//! [`docs/completed/ast-versioning-nullness-proof.md`] for the mechanism proof
//! this productionises.
//!
//! [`docs/ast-versioning-plan.md`]: ../../../../docs/ast-versioning-plan.md
//! [`docs/completed/ast-versioning-nullness-proof.md`]: ../../../../docs/completed/ast-versioning-nullness-proof.md
//! [`WITH_NULL_TYPE`]: super::SyntaxKind::WITH_NULL_TYPE

use crate::language_version::LanguageVersion;
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
/// It gates against the shared [`kind_in_surface`] table, which is **exact for
/// the committed surfaces** (`v8`/`v9`, and any pin `>= 8.0`) but deliberately
/// *partial* below 8.0 — so for a pin in 4.6-7.0 this **under-reports** modelled
/// post-floor kinds (interpolated strings, while-bang, …) rather than guess
/// unverified gating (the D3/D7 limitation; see
/// [`kind_interval`](crate::syntax::kind_interval) "Known gap").
/// Because the one gated kind today is a `Type` kind (nullness), this currently
/// coincides with [`first_out_of_surface_type`].
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
