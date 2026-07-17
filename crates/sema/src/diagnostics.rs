//! Semantic diagnostics produced by the resolver.
//!
//! Today this is a single, *always-sound* diagnostic: [`SemaDiagnosticKind::UseAndRec`]
//! (`use rec`). The broad type-error stream — the gated "Phase 4" work in
//! `docs/type-checker-plan.md` — stays deferred because type errors can be
//! false positives while inference is incomplete (the soundness policy, D5).
//!
//! `use rec` is different in kind: it is *syntactically decidable* — a binding
//! group carries both a `use` keyword and a `rec` keyword
//! ([`LetOrUseExpr::is_use`](borzoi_cst::syntax::LetOrUseExpr::is_use) and
//! [`is_rec`](borzoi_cst::syntax::LetOrUseExpr::is_rec)) — so it needs no
//! name resolution and no type inference and can never be a false positive.
//! That lets it bypass Phase 4's completeness-frontier gating entirely: it is
//! the resolver's first (and so far only) emitted diagnostic.
//!
//! FCS reports the same condition as `FS0821`
//! (`tcBindingCannotBeUseAndRec`, "A binding cannot be marked both 'use' and
//! 'rec'") — but during type-checking (`CheckExpressions.fs`), not parsing, so
//! it belongs here rather than in `borzoi-cst`'s parser.

use rowan::TextRange;

/// A semantic diagnostic over one file, keyed by the source range it concerns.
///
/// A data description, not a formatted string: the [`kind`](Self::kind) is a
/// closed enum the LSP shell renders, so the message text lives in exactly one
/// place ([`SemaDiagnosticKind::message`]) and callers can match on the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemaDiagnostic {
    /// The source range the diagnostic points at — for [`SemaDiagnosticKind::UseAndRec`],
    /// the `use` keyword token (FCS spans the whole `let`/`use` expression; the
    /// keyword is the more useful anchor).
    pub range: TextRange,
    /// What the diagnostic reports.
    pub kind: SemaDiagnosticKind,
}

/// The closed set of conditions the resolver diagnoses. One variant today;
/// matching stays exhaustive as the set grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemaDiagnosticKind {
    /// A binding group is marked both `use` and `rec` (`use rec x = …`). FCS's
    /// `FS0821` (`tcBindingCannotBeUseAndRec`).
    UseAndRec,
}

impl SemaDiagnosticKind {
    /// The human-readable message for this diagnostic. Mirrors FCS's wording
    /// for `FS0821`; the exact text is not contractual (the FCS differential
    /// tests compare *presence*, not message strings).
    pub fn message(self) -> &'static str {
        match self {
            SemaDiagnosticKind::UseAndRec => "A binding cannot be marked both 'use' and 'rec'",
        }
    }
}
