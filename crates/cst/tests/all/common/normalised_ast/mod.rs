//! Shared AST shape for the parser differential harness.
//!
//! Both `fcs-dump ast` output (System.Text.Json `AdjacentTag` encoding of
//! FCS's `ParsedInput`) and our own typed AST (rowan-based) get projected to
//! this shape. The diff is then a plain `assert_eq!`. Per `docs/parser-plan.md`
//! D4: the normaliser elides trivia and ranges by default, so any divergence
//! either way is meaningful.
//!
//! Phase 1 only models what we actually produce: implementation files
//! containing an anonymous module whose decls list may hold zero or more
//! constant-integer expression decls.
//!
//! The projection is split into three submodules over a shared [`model`]:
//! [`from_cst`] walks our rowan AST, [`from_fcs`] walks the FCS JSON dump,
//! and [`decode`] holds the literal/interpolation text decoders both share.

mod decode;
mod from_cst;
mod from_fcs;
mod model;

/// Canonicalise a `match` / `match!` **scrutinee** identifier: a bare `_arg<N>`
/// collapses to `_arg`, dropping FCS's stateful `SynArgNameGenerator` index.
/// Applied identically on both projectors at every
/// [`model::NormalisedExpr::Match`] / [`model::NormalisedExpr::MatchBang`]
/// construction point (see [`decode::canonicalise_synth_arg`]).
///
/// Scoping to the *scrutinee* position keeps it off general value references —
/// the body of `let f _arg1 _arg2 = _arg1, _arg2` keeps its two distinct names,
/// so a projector regression that mis-ordered them is still caught.
///
/// It does also touch a *surface* `match _arg1 with …` (FCS bakes the synthetic
/// `fun`-lowering scaffold into `parsedData`, so a generated `match _arg<N>`
/// and a user-written one are indistinguishable at this projection point). That
/// is sound: a surface scrutinee identifier is read **verbatim from source on
/// both sides** — only *synthetic* args are numbered — so the two projectors
/// always already agree on its name, and collapsing both to `_arg` changes the
/// value but never the equality. The index divergence this exists to absorb can
/// arise *only* for a generated scrutinee, never a source one, so no real
/// difference is masked.
pub(super) fn canonicalise_scrutinee(scrutinee: model::NormalisedExpr) -> model::NormalisedExpr {
    match scrutinee {
        model::NormalisedExpr::Ident(name) => {
            model::NormalisedExpr::Ident(decode::canonicalise_synth_arg(&name))
        }
        other => other,
    }
}

// Each test binary pulls `common` in via `mod common;` and exercises a
// different subset of this surface, so a re-export unused by one binary is
// expected (mirrors the parent module's `#![allow(dead_code)]`). `dead_code`
// doesn't cover re-exports, hence the `unused_imports` allow here too.
#[allow(unused_imports)]
pub use self::{
    decode::{collapse_interp_brace_digraphs, collapse_triple_interp_brace_digraphs},
    from_cst::normalise_parse,
    from_fcs::normalise_fcs_dump,
    model::*,
};
