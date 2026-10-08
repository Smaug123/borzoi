//! Differential test (`parser::parse` vs FCS): the dotted indexer `recv.[i]`
//! under every spacing of its `.` and its opener.
//!
//! FCS's grammar reads `atomicExpr DOT LBRACK …` (`SynExpr.DotIndexedGet`)
//! token by token, so whitespace or a line break on either side of the `.`
//! does not by itself change the construct: `Array. [ yield 1 ]` is a dotted
//! indexer, as `xs .[i]` is. LexFilter does see the layout, though (a `.` at
//! the start of a line is an infix-style continuation, and a `[` that ends a
//! line pushes a context whose body is a sequential block), so which of these
//! spellings FCS accepts, and with what tree, is a matrix question rather than
//! a rule to transcribe. FCS decides every cell's verdict, and we must agree:
//! an accepted cell must give the same normalised tree, and a rejected one
//! must satisfy the recovery relation (`common::recovery`: every declaration
//! outside the damage the same on both sides).
//!
//! Each cell places the access in a context and follows it with another
//! declaration, so a recovery that swallows the rest of the file is a
//! divergence, not only an extra error.

use std::io::Write as _;

use borzoi_cst::parser::parse;
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};
use tempfile::NamedTempFile;

use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};

use crate::common::corpus_manifest::manifest_path;
use crate::common::normalised_ast::{normalise_fcs_dump, normalise_parse};
use crate::common::recovery::{Relation, grade};
use crate::common::{fcs_ast_batch, fcs_parse_had_errors};

/// Receivers: a value, a module-like name, a dotted path, a parenthesised
/// application, an indexer, and an adjacent application.
const RECEIVERS: &[&str] = &["xs", "Array", "a.b", "(f x)", "xs.[0]", "f(x)"];

/// What sits between the receiver and the opener. `{nl}` is a line break to
/// the body's indentation (see [`CONTEXTS`]).
const SEPARATORS: &[&str] = &[".", ". ", " .", " . ", ".{nl}", "{nl}.", "{nl}. "];

/// The bracketed part. `{nl}` is a line break to the body's indentation and
/// `{close}` one to the context's own indentation (the `pos30.fs` layout,
/// where the closer sits under the line that opened it).
const OPENERS: &[&str] = &[
    "[i]",
    "[0..1]",
    "[ yield 1 ]",
    "[{nl}yield 1{nl}yield 2{close}]",
    "[{nl}\"hello\"{close}]",
    "(i)",
    "( i )",
    "()",
];

/// `(template, body indentation, closer indentation)`: `{e}` is the access.
/// Each is followed by another declaration, which must survive.
const CONTEXTS: &[(&str, &str, &str)] = &[
    ("let y = {e}\nlet z = 1\n", "    ", ""),
    (
        "module A =\n    let y = {e}\nmodule B =\n    let z = 1\n",
        "        ",
        "    ",
    ),
    (
        "module A =\n    f(arg = {e})\nmodule B =\n    let z = 1\n",
        "        ",
        "    ",
    ),
    (
        "module A =\n    try\n        let y = {e}\n        h y\n    with _ -> ()\nmodule B =\n    let z = 1\n",
        "            ",
        "        ",
    ),
];

fn cells() -> Vec<String> {
    let mut out = Vec::new();
    for (context, body, close) in CONTEXTS {
        let nl = format!("\n{body}");
        let close = format!("\n{close}");
        for recv in RECEIVERS {
            for sep in SEPARATORS {
                for opener in OPENERS {
                    let access = format!("{recv}{sep}{opener}")
                        .replace("{nl}", &nl)
                        .replace("{close}", &close);
                    out.push(context.replace("{e}", &access));
                }
            }
        }
    }
    out
}

/// How one cell compares with FCS, when the verdicts agree.
enum Cell {
    /// FCS accepts, and the normalised trees are equal.
    Accepted,
    /// FCS rejects, and the recovery relation holds.
    Recovered,
    /// FCS rejects, and an undamaged declaration differs: the first one.
    RecoveryDivergent(String),
}

/// Grade one cell against FCS: the same verdict, then the same tree (both
/// accept) or the recovery relation (both reject). A verdict or accepted-tree
/// divergence panics.
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
    if !fcs_rejects {
        let fcs = normalise_fcs_dump(&json);
        let rust = normalise_parse(&ours);
        assert_eq!(rust, fcs, "AST divergence for source {source:?}");
        return Cell::Accepted;
    }
    let verdict = grade(&ours, &json, source).expect("FCS record is readable");
    match verdict.relation {
        Relation::Exact | Relation::OutsideDamage => Cell::Recovered,
        Relation::Divergent => Cell::RecoveryDivergent(
            verdict
                .first_divergence
                .unwrap_or_else(|| "(no divergence named)".to_string()),
        ),
    }
}

/// Every cell through [`grade_cell`]. A verdict or accepted-tree divergence
/// fails outright, and all of them are reported together; a recovered tree
/// that diverges is pinned, cell by cell, in
/// `tests/manifests/dot_index_spacing.txt`.
#[test]
fn diff_dot_index_spacing_matrix() {
    let mut total = 0;
    let mut accepted = 0;
    let mut failures = Vec::new();
    let mut recovery_divergent = Vec::new();
    for source in cells() {
        total += 1;
        match catch_unwind_silent(|| grade_cell(&source)) {
            Ok(Cell::Accepted) => accepted += 1,
            Ok(Cell::Recovered) => {}
            Ok(Cell::RecoveryDivergent(first)) => {
                recovery_divergent.push(format!("{source:?} {first}"));
            }
            Err(_) => {
                let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
                let first_line = message.lines().next().unwrap_or_default().to_owned();
                failures.push(first_line);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} dotted-indexer spacing cells diverge from FCS:\n{}",
        failures.len(),
        failures.join("\n"),
    );
    assert!(
        accepted > 0 && accepted < total,
        "the sweep should both accept and reject, but accepted {accepted} of {total}",
    );
    let manifest = Manifest::from_entries(recovery_divergent).expect("one line per cell");
    check(
        &manifest_path("dot_index_spacing"),
        &manifest,
        &format!(
            "{UPDATE_ENV}=1 nix develop -c cargo test -p borzoi-cst --test all \
             parser_diff_dot_index_spacing::"
        ),
    );
}
