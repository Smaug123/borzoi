//! Differential test (`parser::parse` vs FCS): expressions whose keywords reach
//! the parser **raw**, generated two constructs deep.
//!
//! Inside a module header's attribute list (`module [<A(…)>] M`) FCS's lex
//! filter pushes no offside contexts, so `then`, `else`, `with`, `fun` and
//! `function` arrive as the raw tokens `THEN`/`ELSE`/`WITH`/`FUN`/`FUNCTION`
//! rather than their `OTHEN`/`OELSE`/`OWITH`/`OFUN`/`OFUNCTION` relabels, and FCS
//! parses them with its non-offside productions (`THEN declExpr`, `WITH
//! withPatternClauses`, `FUN atomicPatterns RARROW typedSequentialExprBlock`).
//! Those productions differ from the offside ones in where a body ends (a raw
//! `then` body is a `declExpr`, so it stops at `;`), and nothing closes them.
//!
//! Hand-written cases sample those boundaries badly, so this composes a small
//! grammar of constructs: every construct with leaf holes, and every construct
//! with one hole filled by another construct. Each cell goes to
//! [`assert_parse_verdicts_match`], so a cell we wrongly reject, wrongly accept,
//! or accept with a divergent tree fails. Nothing here hard-codes which cells
//! FCS accepts.

use crate::common::assert_parse_verdicts_match;
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};

/// The hole marker in [`CONSTRUCTS`]. No construct spells `@` itself.
const HOLE: &str = "@";

/// Expression constructs, each with one or more [`HOLE`]s. They span the
/// keywords that have a raw spelling, the separators and operators whose
/// precedence decides where a raw body ends (`;`, `,`, `then`, an infix, a type
/// annotation), the constructs that are raw in every layout (`let … in`,
/// `lazy`, verbose `while … done`), and the loops FCS rejects without `done`.
const CONSTRUCTS: &[&str] = &[
    "if @ then @",
    "if @ then @ else @",
    "if @ then @ elif @ then @ else @",
    "match @ with _ -> @",
    "match @ with | 1 -> @ | _ -> @",
    "match @ with _ when @ -> @",
    "try @ with _ -> @",
    "try @ finally @",
    "fun x -> @",
    "fun x y -> @",
    "function _ -> @",
    "function | 1 -> @ | _ -> @",
    "@, @",
    "@; @",
    "@ then @",
    "@ + @",
    "@ : int",
    "(@)",
    "let x = @ in @",
    "lazy @",
    "while @ do @ done",
    "while @ do @",
    "for x in @ do @ done",
    "for x in @ do @",
];

/// The leaf every hole not being expanded is filled with.
const LEAF: &str = "1";

/// `template` with its `n`th hole replaced by `fill` and every other hole by
/// [`LEAF`].
fn fill_nth(template: &str, n: usize, fill: &str) -> String {
    let mut out = String::new();
    for (i, piece) in template.split(HOLE).enumerate() {
        if i > 0 {
            out.push_str(if i - 1 == n { fill } else { LEAF });
        }
        out.push_str(piece);
    }
    out
}

/// Every expression in the grid: each construct with leaf holes, then each
/// construct with one hole holding another leaf-filled construct.
fn expressions() -> Vec<String> {
    let leafy: Vec<String> = CONSTRUCTS
        .iter()
        .map(|c| fill_nth(c, usize::MAX, LEAF))
        .collect();
    let mut out = leafy.clone();
    for outer in CONSTRUCTS {
        let holes = outer.matches(HOLE).count();
        for n in 0..holes {
            for inner in &leafy {
                out.push(fill_nth(outer, n, inner));
            }
        }
    }
    out
}

/// Run every cell through the verdict oracle, collecting (rather than stopping
/// at) divergences, and assert both verdicts were exercised.
///
/// `known` lists cells that diverge for reasons outside this grid's subject.
/// Each must still diverge, so the list cannot outlive the defect it records.
fn sweep(what: &str, cells: impl IntoIterator<Item = String>, known: &[&str]) {
    let mut total = 0;
    let mut accepted = 0;
    let mut failures = Vec::new();
    let mut known_now_agreeing = Vec::new();
    for source in cells {
        total += 1;
        let is_known = known.contains(&source.as_str());
        match catch_unwind_silent(|| assert_parse_verdicts_match(&source)) {
            Ok(verdict) => {
                if verdict {
                    accepted += 1;
                }
                if is_known {
                    known_now_agreeing.push(source);
                }
            }
            Err(_) => {
                let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
                if !is_known {
                    let first_line = message.lines().next().unwrap_or_default().to_owned();
                    failures.push(first_line);
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} {what} cells diverge from FCS:\n{}",
        failures.len(),
        failures.join("\n"),
    );
    assert!(
        known_now_agreeing.is_empty(),
        "these {what} cells are listed as known divergences but now agree with FCS; \
         remove them from the list:\n{known_now_agreeing:#?}",
    );
    // All-accept or all-reject would pass every cell while testing nothing
    // about the other verdict; which cell falls where is FCS's call.
    assert!(
        accepted > 0 && accepted < total,
        "the {what} sweep should both accept and reject, but accepted {accepted} of {total}",
    );
}

/// Every grid expression as the argument of a whole-file module header's
/// attribute (`module [<A(…)>] M`), where every keyword in it reaches the
/// parser raw.
#[test]
fn diff_raw_keyword_grid_in_module_header_attribute() {
    sweep(
        "module-header attribute",
        expressions()
            .into_iter()
            .map(|e| format!("module [<A({e})>] M\nlet x = 1\n")),
        &[],
    );
}

/// The leaf-filled constructs as the *second* element of a tuple argument,
/// after a comma (`module [<A(1, …)>] M`), and in a nested module's header.
#[test]
fn diff_raw_keyword_grid_in_other_header_positions() {
    let leafy: Vec<String> = CONSTRUCTS
        .iter()
        .map(|c| fill_nth(c, usize::MAX, LEAF))
        .collect();
    sweep(
        "tuple-element and nested-header attribute",
        leafy.iter().flat_map(|e| {
            [
                format!("module [<A(1, {e})>] M\nlet x = 1\n"),
                format!("module [<A({e})>] M =\n  let x = 1\n"),
                format!("namespace N\nmodule [<A({e})>] M =\n  let x = 1\n"),
            ]
        }),
        &[],
    );
}

/// Offside-layout cells that diverge from FCS independently of the raw
/// spellings (they diverge identically without the raw-keyword productions):
/// a `try` directly nested in another `try`'s body, which FCS rejects; and a
/// `: T` annotation after a `then` sequence, which FCS applies to the whole
/// sequence.
const OFFSIDE_CONTROL_KNOWN: &[&str] = &[
    "let v = (try try 1 with _ -> 1 with _ -> 1)\n",
    "let v = (try try 1 finally 1 with _ -> 1)\n",
    "let v = (try try 1 with _ -> 1 finally 1)\n",
    "let v = (try try 1 finally 1 finally 1)\n",
    "let v = (1 then 1 : int)\n",
];

/// The same expressions in an ordinary offside position, as a control: there
/// the lex filter relabels every keyword, so these cells exercise the offside
/// productions the raw ones sit beside.
#[test]
fn diff_raw_keyword_grid_offside_control() {
    sweep(
        "offside control",
        expressions()
            .into_iter()
            .map(|e| format!("let v = ({e})\n")),
        OFFSIDE_CONTROL_KNOWN,
    );
}
