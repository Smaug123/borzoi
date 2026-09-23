//! Differential test (`parser::parse` vs FCS): the *matrix* of infix operators —
//! every operator spelling alone, every ordered pair, and every operator at
//! every undentation column of a continuation line.
//!
//! Operator handling has two parts that hand-written cases sample badly:
//!
//! * **precedence and associativity**, decided per spelling by FCS's lexer
//!   (`lex.fsl`'s operator classes, keyed on the leading characters) and the
//!   `%left`/`%right` table in `pars.fsy`;
//! * **the infix offside grace**, by which LexFilter lets a line that *starts*
//!   with an infix operator sit left of its context by the operator's width
//!   plus one (`LexFilter.fs`, `infixTokenLength`).
//!
//! Both are tables keyed on the operator's spelling, so the natural test is the
//! cross-product of spellings, with FCS asked for every verdict: each cell goes
//! to [`assert_parse_verdicts_match`], so a cell we wrongly reject, wrongly
//! accept, or accept with a divergent tree all fail. Nothing here hard-codes
//! which cells FCS accepts.
//!
//! Every cell runs, and the failures are reported together: one divergence in a
//! table-driven rule usually means a family of them, and the family is the
//! useful report.

use crate::common::assert_parse_verdicts_match;
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};

/// Operator spellings spanning FCS's infix classes: each class of leading
/// character (`lex.fsl`'s `INFIX_*_OP` families), the keyword operators, the
/// fixed-meaning tokens (`::`, `:=`, `&&`, `||`, `$`, `&`, `or`), the
/// quotation brackets, a few multi-character spellings whose class is decided
/// by their first character alone, and spellings carrying the `:`/`$` that
/// FCS's lexer forbids inside an operator name (except `:` after a `>` head).
const OPS: &[&str] = &[
    "+", "-", "*", "/", "%", "**", "@", "^", "::", "|>", "<|", ">>", "<<", "=", "<>", "<", ">",
    "<=", ">=", "&&", "||", "&&&", "|||", "^^^", "<<<", ">>>", ":=", "$", ".+", "?+", "+.", "-.",
    ".*", ">>=", ">=>", "<*>", "<!>", "%%", "!=", "==>", "-->", "|||>", "@@", "^+", "@+", "$+",
    ".@", "mod", "land", "lor", "lxor", "lsl", "lsr", "asr", "or", "&", "<@", "@>", "+:", ">:",
    ".>:", "?$", "$$", "=$",
];

/// Run every cell through the verdict oracle, collecting (rather than stopping
/// at) divergences, and assert both verdicts were exercised.
fn sweep(what: &str, cells: impl IntoIterator<Item = String>) {
    let mut total = 0;
    let mut accepted = 0;
    let mut failures = Vec::new();
    for source in cells {
        total += 1;
        match catch_unwind_silent(|| assert_parse_verdicts_match(&source)) {
            Ok(true) => accepted += 1,
            Ok(false) => {}
            Err(_) => {
                let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
                let first_line = message.lines().next().unwrap_or_default().to_owned();
                failures.push(first_line);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} {what} cells diverge from FCS:\n{}",
        failures.len(),
        failures.join("\n"),
    );
    // All-accept or all-reject would pass every cell while testing nothing
    // about the other verdict; which cell falls where is FCS's call.
    assert!(
        accepted > 0 && accepted < total,
        "the {what} sweep should both accept and reject, but accepted {accepted} of {total}",
    );
}

#[test]
fn diff_single_operator_matrix() {
    sweep(
        "single-operator",
        OPS.iter().map(|op| format!("let x = a {op} b\n")),
    );
}

/// Every ordered pair: `a OP1 b OP2 c` parses as `(a OP1 b) OP2 c` or
/// `a OP1 (b OP2 c)` according to the two spellings' relative precedence and
/// shared associativity — exactly the table a hand-picked chain samples.
#[test]
fn diff_operator_pair_matrix() {
    sweep(
        "operator-pair",
        OPS.iter()
            .flat_map(|a| OPS.iter().map(move |b| format!("let x = a {a} b {b} c\n"))),
    );
}

/// A continuation line opening with each operator, at every column from the
/// margin to the context's own column (6): the infix grace decides, per
/// spelling, how far left of the context the line may start.
#[test]
fn diff_infix_undentation_matrix() {
    sweep(
        "infix-undentation",
        OPS.iter().flat_map(|op| {
            (0..=6).map(move |col| format!("let x =\n      a\n{}{op} b\n", " ".repeat(col)))
        }),
    );
}
