//! Differential test (`parser::parse` vs FCS): an argument that ends in an
//! adjacent application, `g f(x)`.
//!
//! FCS's `atomicExpr` carries a flag that is set when the expression ends in a
//! high-precedence application (`pars.fsy`, `atomicExpr
//! HIGH_PRECEDENCE_PAREN_APP atomicExpr` sets it; `.member`, `?name` and a
//! prefix operator pass the receiver's flag on; a type application and a list
//! literal clear it). `argExpr` reports FS0597 ("Successive arguments should be
//! separated by spaces or tupled…") on an argument whose flag is set, at the
//! argument's range, or at the operator of an `ADJACENT_PREFIX_OP` argument
//! (`g -f(x)`). It is a parse error that keeps the tree: FCS builds the same
//! `App` it would have built without the error.
//!
//! So each cell is graded three ways. The verdicts must agree. Where FCS's only
//! errors are FS0597, the normalised trees must be equal and our error spans
//! must be exactly FCS's FS0597 spans, each with the FS0597 message. Where FCS
//! reports anything else, the recovery relation must hold (`common::recovery`).
//!
//! The axes are the argument (which forms carry the flag is FCS's to decide,
//! not ours to transcribe), the application head, the argument's position in
//! the application, and the context the application sits in. Each cell is
//! followed by another declaration, so a recovery that swallows the rest of
//! the file is a divergence.

use std::collections::BTreeSet;
use std::io::Write as _;

use borzoi_cst::parser::{SUCCESSIVE_ARGS_MESSAGE, parse};
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};
use serde::Deserialize;
use tempfile::NamedTempFile;

use crate::common::normalised_ast::{normalise_fcs_dump, normalise_parse};
use crate::common::recovery::{Relation, grade};
use crate::common::{LineIndex, fcs_ast_batch, fcs_parse_had_errors};

/// Candidate arguments. Some end in an adjacent application and some do not;
/// some reach one through a member access, an indexer, a type application or
/// a prefix operator.
const ARGS: &[&str] = &[
    // Adjacent applications.
    "f(x)",
    "f()",
    "f(x, y)",
    "f(x)(y)",
    "f.M(x)",
    "f.M()",
    "f<int>(x)",
    "f.M<int>(x)",
    "(f)(x)",
    "f[x](y)",
    // Something built on one.
    "f(x).P",
    "f(x).P(y)",
    "f(x).[0]",
    "f(x)[0]",
    "f(x)?p",
    "f(x).M<int>",
    "f(x).P.Q",
    "f(x).P[0]",
    "f.M(x).P<int>",
    // Prefix operators over one.
    "-f(x)",
    "+f(x)",
    "!f(x)",
    "~~~f(x)",
    "%f(x)",
    "&f(x)",
    "&&f(x)",
    "-f(x).P",
    "!f(x).P",
    // Controls: no adjacent application at the end.
    "f",
    "f[x]",
    "f[x].P",
    "xs.[0]",
    "f<int>",
    "(f x)",
    "(f(x))",
    "f (x)",
    "-f",
    "!f",
    "-(f x)",
    "_.M(x)",
    "\"s\".Length",
    "\"s\".M(x)",
    "[1](x)",
    "f(x)<int>",
];

/// Application heads.
const HEADS: &[&str] = &["g", "obj.M", "g<int>", "(g)", "g(a)"];

/// Where the argument sits among the application's arguments. `{h}` is the
/// head and `{a}` the argument.
const POSITIONS: &[&str] = &["{h} {a}", "{h} {a} y", "{h} y {a}", "{h} y {a} z"];

/// The contexts the application sits in. `{e}` is the application. Each is
/// followed by another declaration, which must survive.
const CONTEXTS: &[&str] = &[
    "let v = {e}\nlet z = 1\n",
    "let v = x |> {e}\nlet z = 1\n",
    "let v = {e} |> k\nlet z = 1\n",
    "let v = h ({e}, 1)\nlet z = 1\n",
    "module A =\n    {e}\nmodule B =\n    let z = 1\n",
    "let v = obj.N(arg = {e})\nlet z = 1\n",
];

fn cells() -> Vec<String> {
    let mut out = Vec::new();
    for context in CONTEXTS {
        for position in POSITIONS {
            for head in HEADS {
                for arg in ARGS {
                    let app = position.replace("{h}", head).replace("{a}", arg);
                    out.push(context.replace("{e}", &app));
                }
            }
        }
    }
    out
}

/// The FCS diagnostic fields this test reads.
#[derive(Deserialize)]
struct Dump {
    #[serde(rename = "Diagnostics")]
    diagnostics: Vec<Diagnostic>,
}

#[derive(Deserialize)]
struct Diagnostic {
    #[serde(rename = "ErrorNumber")]
    error_number: i64,
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(rename = "Range")]
    range: Range,
}

#[derive(Deserialize)]
struct Range {
    #[serde(rename = "Start")]
    start: Pos,
    #[serde(rename = "End")]
    end: Pos,
}

#[derive(Deserialize)]
struct Pos {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Col")]
    col: u32,
}

/// How one cell compares with FCS, when it does not fail outright.
enum Cell {
    /// Neither side reports an error, and the trees are equal.
    Accepted,
    /// FCS's only errors are FS0597; the trees are equal and our errors are
    /// exactly FCS's, at the same spans.
    Successive,
    /// FCS reports another error, and the recovery relation holds.
    Recovered,
}

fn grade_cell(source: &str) -> Cell {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let json = fcs_ast_batch(tmp.path());
    let fcs_rejects = fcs_parse_had_errors(&json);
    let ours = parse(source);
    let we_reject = !ours.errors.is_empty();
    assert_eq!(
        we_reject, fcs_rejects,
        "parse-verdict divergence for {source:?}: we reject: {we_reject}, our errors: {:?}",
        ours.errors,
    );
    let dump: Dump = serde_json::from_str(&json).expect("fcs-dump diagnostics shape");
    let errors: Vec<&Diagnostic> = dump
        .diagnostics
        .iter()
        .filter(|d| d.severity == "Error")
        .collect();
    if errors.iter().all(|d| d.error_number == 597) {
        let fcs = normalise_fcs_dump(&json);
        let rust = normalise_parse(&ours);
        assert_eq!(rust, fcs, "AST divergence for source {source:?}");
        let index = LineIndex::new(source);
        let fcs_spans: BTreeSet<(usize, usize)> = errors
            .iter()
            .map(|d| {
                (
                    index.offset(d.range.start.line, d.range.start.col),
                    index.offset(d.range.end.line, d.range.end.col),
                )
            })
            .collect();
        let our_spans: BTreeSet<(usize, usize)> = ours
            .errors
            .iter()
            .map(|e| (e.span.start, e.span.end))
            .collect();
        assert_eq!(
            our_spans, fcs_spans,
            "FS0597 spans differ for {source:?}; our errors: {:?}",
            ours.errors,
        );
        assert!(
            ours.errors
                .iter()
                .all(|e| e.message == SUCCESSIVE_ARGS_MESSAGE),
            "FCS reports only FS0597 for {source:?}, but our errors are {:?}",
            ours.errors,
        );
        return if errors.is_empty() {
            Cell::Accepted
        } else {
            Cell::Successive
        };
    }
    let verdict = grade(&ours, &json, source).expect("FCS record is readable");
    assert!(
        verdict.relation != Relation::Divergent,
        "recovery divergence for {source:?}: {}",
        verdict
            .first_divergence
            .unwrap_or_else(|| "(no divergence named)".to_string()),
    );
    Cell::Recovered
}

/// Every cell through [`grade_cell`]. Every divergence fails, and all of
/// them are reported together.
#[test]
fn diff_successive_args_matrix() {
    let mut total = 0;
    let mut accepted = 0;
    let mut successive = 0;
    let mut recovered = 0;
    let mut failures = Vec::new();
    for source in cells() {
        total += 1;
        match catch_unwind_silent(|| grade_cell(&source)) {
            Ok(Cell::Accepted) => accepted += 1,
            Ok(Cell::Successive) => successive += 1,
            Ok(Cell::Recovered) => recovered += 1,
            Err(_) => {
                let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
                let first_line = message.lines().next().unwrap_or_default().to_owned();
                failures.push(first_line);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} successive-argument cells diverge from FCS:\n{}",
        failures.len(),
        failures.join("\n"),
    );
    eprintln!("{total} cells: {accepted} clean, {successive} FS0597 only, {recovered} recovered");
    // The matrix must exercise all three outcomes, or it has stopped testing
    // the rule: the clean cells, the FS0597 cells, and the rest.
    assert!(
        accepted > 0 && successive > 0 && recovered > 0,
        "accepted {accepted}, FS0597-only {successive}, recovered {recovered}, of {total}",
    );
}
