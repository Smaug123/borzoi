//! Semantic diagnostics produced by the resolver.
//!
//! Each is reported only where it is *certain* — FCS reports the same error at
//! the same place — so a missing diagnostic is a gap, never a wrong one. The
//! broad type-error stream — the gated "Phase 4" work in
//! `docs/type-checker-plan.md` — stays deferred because type errors can be
//! false positives while inference is incomplete (the soundness policy, D5).
//!
//! - [`SemaDiagnosticKind::UseAndRec`] (`use rec`) is *syntactically
//!   decidable* — a binding group carries both a `use` keyword and a `rec`
//!   keyword ([`LetOrUseExpr::is_use`](borzoi_cst::syntax::LetOrUseExpr::is_use)
//!   and [`is_rec`](borzoi_cst::syntax::LetOrUseExpr::is_rec)) — so it needs no
//!   name resolution and no type inference. FCS reports the same condition as
//!   `FS0821` (`tcBindingCannotBeUseAndRec`, "A binding cannot be marked both
//!   'use' and 'rec'") — but during type-checking (`CheckExpressions.fs`), not
//!   parsing, so it belongs here rather than in `borzoi-cst`'s parser.
//! - [`SemaDiagnosticKind::OpenOfRequireQualifiedAccessModule`] (`FS0892`)
//!   needs name resolution — which modules the `open` names, and whether one
//!   certainly carries FSharp.Core's attribute — and is reported only where
//!   both are certain (`resolve/require_qualified.rs`).

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
    /// keyword is the more useful anchor); for
    /// [`SemaDiagnosticKind::OpenOfRequireQualifiedAccessModule`], the opened
    /// path, exactly FCS's range.
    pub range: TextRange,
    /// What the diagnostic reports.
    pub kind: SemaDiagnosticKind,
}

/// The closed set of conditions the resolver diagnoses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemaDiagnosticKind {
    /// A binding group is marked both `use` and `rec` (`use rec x = …`). FCS's
    /// `FS0821` (`tcBindingCannotBeUseAndRec`).
    UseAndRec,
    /// An `open` names a module marked `[<RequireQualifiedAccess>]`. FCS's
    /// `FS0892` (`tcModuleRequiresQualifiedAccess`), an error reported on the
    /// opened path; FCS still opens the module.
    OpenOfRequireQualifiedAccessModule,
}

impl SemaDiagnosticKind {
    /// The human-readable message for this diagnostic. Mirrors FCS's wording
    /// for `FS0821`; the exact text is not contractual (the FCS differential
    /// tests compare *presence*, not message strings).
    pub fn message(self) -> &'static str {
        match self {
            SemaDiagnosticKind::UseAndRec => "A binding cannot be marked both 'use' and 'rec'",
            SemaDiagnosticKind::OpenOfRequireQualifiedAccessModule => {
                "This declaration opens a module which is marked as 'RequireQualifiedAccess'. \
                 Adjust your code to use qualified references to the elements of the module \
                 instead, e.g. 'List.map' instead of 'map'."
            }
        }
    }
}
