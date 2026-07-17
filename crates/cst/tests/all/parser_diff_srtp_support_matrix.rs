//! Differential test (`parser::parse` vs FCS): the *matrix* of SRTP supports —
//! every support shape × every operand shape × both support contexts.
//!
//! F# has two `or`-separated SRTP supports, and they do **not** take the same
//! operand:
//!
//! * the member *constraint*'s `typeAlts` (`pars.fsy:2705`) —
//!   `when (^T or Witnesses) : (member …)` — takes `appTypeWithoutNull`;
//! * the trait-call *expression*'s `typarAlts` (`pars.fsy:5547`) —
//!   `((^T or int) : (static member …) …)` — takes `appTypeCanBeNullable`,
//!   which is an `appTypeWithoutNull` plus an optional `| null`.
//!
//! Every bug this file was written in response to was a *silent disagreement
//! with that grammar*, invisible from our own tree, and each needed a human to
//! spot:
//!
//! 1. the parser demanded a `^a` typar for *every* alternative, so it rejected
//!    `(^T or int)`, which FCS accepts;
//! 2. fixed by reusing the constraint's operand — which dropped `| null` in the
//!    expression, where FCS accepts it;
//! 3. the constraint operand called the postfix-run production, silently losing
//!    the `'U :> obj` subtype shorthand that `appTypeWithoutNull` includes;
//! 4. a quoted first typar was admitted unconditionally, committing the singleton
//!    `(('T) : …)` that FCS rejects (while accepting `((^T) : …)`).
//!
//! All four are one mistake: assuming two positions share a production. And (4)
//! is the sharpest lesson about *this file* — it slipped through a first version
//! of the matrix that varied the operand while holding the support head fixed.
//! **A dimension you hold constant is a dimension you are not testing.** So the
//! sweep below is a genuine cross-product: the first alternative's sigil, the
//! operand after the `or`, and the context all vary together, and the degenerate
//! heads (no `or` at all) get their own sweep.
//!
//! Each cell goes to [`assert_parse_verdicts_match`], which asks FCS for the
//! verdict rather than asserting one: a cell we wrongly reject, wrongly accept, or
//! accept with a divergent tree all fail. The `| null` asymmetry between the two
//! contexts is therefore not a fact this test hard-codes — it is a fact FCS
//! supplies, and would keep supplying if the grammar changed under us.

use crate::common::assert_parse_verdicts_match;

/// The two SRTP support *contexts*. Same support syntax, different production —
/// which is the whole point of sweeping them together.
#[derive(Clone, Copy)]
enum Context {
    /// `typarAlts`, operand `appTypeCanBeNullable`.
    TraitCall,
    /// `typeAlts`, operand `appTypeWithoutNull`.
    WhenConstraint,
}

impl Context {
    fn source(self, support: &str) -> String {
        match self {
            Context::TraitCall => {
                format!("let inline f (x: 'T) = ({support} : (static member A: int) ())\n")
            }
            Context::WhenConstraint => {
                format!("type C< ^T when {support} : (static member A: int)> = class end\n")
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Context::TraitCall => "trait-call",
            Context::WhenConstraint => "constraint",
        }
    }
}

const CONTEXTS: &[Context] = &[Context::TraitCall, Context::WhenConstraint];

/// The typar that opens a parenthesised alternatives list — `typarAlts`' base
/// case. Both sigils, because they are *not* interchangeable: FCS rejects the
/// singleton `(('T) : …)` but accepts `(('T or int) : …)`.
const FIRST_ALTS: &[&str] = &["^T", "'T"];

/// The operand shapes an alternative can take, spanning the productions of FCS's
/// `appTypeWithoutNull` (`pars.fsy:6371`) plus the `| null` suffix that only
/// `appTypeCanBeNullable` (`pars.fsy:6357`) adds. Deliberately includes shapes
/// that nest the brackets the trait-call commit scan depth-counts, and shapes FCS
/// *rejects* in one or both contexts — a rejected cell is a real assertion (we
/// must reject it too, cleanly, without panicking), not a gap.
const OPERANDS: &[&str] = &[
    // Typars.
    "^b",
    "'U",
    // Concrete leaf and long-ident types.
    "int",
    "System.Int32",
    // Postfix and prefix applications.
    "int list",
    "int list list",
    "Set<int>",
    "Map<int, string list>",
    // Arrays — the `[`/`]` the commit scan depth-counts.
    "int[]",
    "int[][]",
    // Parenthesised operands — the `(`/`)` the commit scan depth-counts, so the
    // support's *own* closer is the one at depth zero.
    "(int * string)",
    "(int -> string)",
    // The subtype-constrained typar (`typar COLON_GREATER typ`, inside
    // `appTypeWithoutNull` — the production whose omission was bug 3).
    "'U :> obj",
    // The nullable suffix — `appTypeCanBeNullable` only, so FCS accepts these in
    // the trait call and rejects them in the constraint (bug 2).
    "string | null",
    "int list | null",
    // Beyond either operand: an arrow is not an `appType` (FCS: "Unexpected
    // symbol '->' … Expected 'or', ')'"), so both contexts must reject.
    "int -> string",
    // Types that the flat-typar assumption made unthinkable here. Enumerate and
    // let FCS rule; each is an assertion either way.
    "#System.IDisposable",
    "_",
    "struct (int * string)",
    "{| X: int |}",
    "int[,]",
    "Foo<int -> string>",
    "Foo<string | null>",
    "Foo<(int * string)>",
    "Foo<{| X: int |}>",
    "int^2",
    // Not types at all — a literal in type position. Both contexts must reject.
    "42",
    "null",
    "true",
];

/// The support shapes with **no** `or` — the dimension whose being held constant
/// hid bug 4. A bare typar, the parenthesised singletons (legal for `^`, not for
/// `'`), and the degenerate lists.
const DEGENERATE_HEADS: &[&str] = &[
    "^T", "'T", "(^T)", "('T)", "((^T))", "()", "(or)", "(^T or)",
];

/// Assert a sweep exercised *both* verdicts. All-accept or all-reject would pass
/// every cell while testing nothing about the other side; which cell falls where
/// is FCS's call, so this pins only that both sides stay populated.
fn assert_both_verdicts_exercised(what: &str, accepted: usize, total: usize) {
    assert!(
        accepted > 0 && accepted < total,
        "the {what} sweep should both accept and reject, but accepted {accepted} \
         of {total}",
    );
}

/// The cross-product: first-alternative sigil × operand × context. No dimension
/// is held constant, so an interaction between two of them (a quoted head *and* a
/// nullable operand, say) cannot hide in a cell nobody generated.
#[test]
fn diff_srtp_support_alternatives_matrix() {
    for context in CONTEXTS {
        let mut accepted = 0;
        let mut total = 0;
        for first in FIRST_ALTS {
            for operand in OPERANDS {
                let source = context.source(&format!("({first} or {operand})"));
                if assert_parse_verdicts_match(&source) {
                    accepted += 1;
                }
                total += 1;
            }
        }
        assert_both_verdicts_exercised(context.name(), accepted, total);
    }
}

/// The `or`-less supports, in both contexts.
#[test]
fn diff_srtp_support_degenerate_heads_matrix() {
    for context in CONTEXTS {
        let accepted = DEGENERATE_HEADS
            .iter()
            .filter(|head| assert_parse_verdicts_match(&context.source(head)))
            .count();
        assert_both_verdicts_exercised(
            &format!("{} degenerate-head", context.name()),
            accepted,
            DEGENERATE_HEADS.len(),
        );
    }
}
