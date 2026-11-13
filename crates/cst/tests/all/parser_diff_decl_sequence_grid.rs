//! Differential test (`parser::parse` vs FCS): two module-level declarations
//! in sequence, over every pair of declaration forms, the column the second
//! sits at relative to the first, and the body they share.
//!
//! FCS's `moduleDefns` (`pars.fsy:1273`) lets one definition follow another
//! with no separator between them (`moduleDefnOrDirective moduleDefns`), but
//! an expression needs one before whatever follows it (`declExpr topSeparators
//! moduleDefnsOrExpr`). Whether the lex filter puts a separator there depends
//! on the second declaration's column: aligned with the first it gets an
//! `OBLOCKSEP`, deeper it gets none, and shallower (inside a nested body) it
//! closes the first's block. An attribute list carries its own optional
//! separator (`attributeList: … GREATER_RBRACK opt_OBLOCKSEP`), so it may sit on
//! a line of its own at another column from the declaration it decorates. Each
//! cell is graded by [`assert_parse_verdicts_match`]: the verdicts, and the
//! trees where both accept. Nothing here hard-codes which cells FCS accepts.
//!
//! Every divergent cell is pinned in `tests/manifests/decl_sequence_grid.txt`,
//! and each must be one where `main`'s parser diverges too
//! (`decl_sequence_grid.base.txt`, from `tools/cst-grid-base.sh`), so a parser
//! change can only move cells towards FCS; #648 records the main families.
//! [`every_declaration_pair_streams`] grades the same cells' lex-filter
//! streams, where an agreeing verdict can hide a divergence.

use crate::common::assert_parse_verdicts_match;
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};

/// Declarations, one `(column offset, text)` per source line, the offset
/// relative to the declaration's column. An attribute list may sit on its own
/// line deeper or shallower than the declaration it decorates.
const DECLS: &[&[(isize, &str)]] = &[
    &[(0, "let x = 1")],
    &[(0, "let f () ="), (4, "1")],
    &[(0, "open System")],
    &[(0, "type T = int")],
    &[(0, "type U ="), (4, "| A"), (4, "| B")],
    &[(0, "exception E")],
    &[(0, "module N = let y = 2")],
    &[(0, "module N ="), (4, "let y = 2")],
    &[(0, "[<AutoOpen>]"), (0, "module N = let y = 2")],
    &[(1, "[<AutoOpen>]"), (0, "module N = let y = 2")],
    &[(0, "[<AutoOpen>]"), (1, "module N = let y = 2")],
    &[(0, "[<Literal>]"), (0, "let L = 1")],
    &[(1, "[<Literal>]"), (0, "let L = 1")],
    &[(0, "[<Literal>] let L = 1")],
    &[(0, "let x = 1 in ()")],
    &[(0, "let x = 1 in")],
    &[(0, "type T = class end")],
    &[(0, "type T with"), (4, "member x.M = 1")],
    &[(0, "exception E with"), (4, "member x.M = 1")],
    &[(0, "module N = begin let y = 1 end")],
    &[(0, "extern int f(int x)")],
    &[(0, "#nowarn \"44\"")],
    &[(0, "do ()")],
    &[(0, "printfn \"x\"")],
    &[(0, "[<assembly: A>] ()")],
    &[(0, "[<assembly: A>] do ()")],
    &[(0, "[<assembly: A>] let x = 1 in ()")],
    // A definition with debris after it, which FCS recovers from only at a
    // separator.
    &[(0, "open System x")],
    &[(0, "open System )")],
    &[(0, "open System and")],
    &[(0, "open type System.String")],
];

/// A body to hold the two declarations: its header, and the column its
/// declarations start at.
const BODIES: &[(&str, usize)] = &[
    ("", 0),
    ("module M\n", 0),
    ("namespace N\n", 0),
    ("module M =\n", 4),
];

/// The second declaration's column relative to the first's, or `None` for the
/// same line (after a single space).
const OFFSETS: &[Option<isize>] = &[None, Some(-1), Some(0), Some(1), Some(4)];

/// `decl`'s lines with the declaration at column `col`, or `None` when a line
/// would fall left of column 0.
fn render(decl: &[(isize, &str)], col: usize) -> Option<String> {
    let mut out = String::new();
    for &(offset, text) in decl {
        out.push_str(&" ".repeat(col.checked_add_signed(offset)?));
        out.push_str(text);
        out.push('\n');
    }
    Some(out)
}

fn cells() -> Vec<String> {
    let mut out = Vec::new();
    for &(header, col) in BODIES {
        for &first in DECLS {
            for &second in DECLS {
                // A namespace takes no expressions or `let`s.
                if header.starts_with("namespace") && (is_value(first) || is_value(second)) {
                    continue;
                }
                for &offset in OFFSETS {
                    let Some(first_src) = render(first, col) else {
                        continue;
                    };
                    let second_src = match offset {
                        // Same line: a one-line declaration after a one-line one.
                        None if first.len() == 1 && second.len() == 1 => {
                            format!(" {}\n", second[0].1)
                        }
                        None => continue,
                        Some(o) => {
                            let Some(c) = col.checked_add_signed(o) else {
                                continue;
                            };
                            let Some(s) = render(second, c) else {
                                continue;
                            };
                            s
                        }
                    };
                    let mut source = String::from(header);
                    source.push_str(&first_src);
                    if offset.is_none() {
                        source.pop();
                    }
                    source.push_str(&second_src);
                    out.push(source);
                }
            }
        }
    }
    out
}

/// A declaration a namespace does not take.
fn is_value(decl: &[(isize, &str)]) -> bool {
    let head = decl.last().map_or("", |&(_, text)| text);
    head.starts_with("let ")
        || head.starts_with("do ")
        || head.starts_with("printfn")
        || head.starts_with("[<assembly")
}

#[test]
fn every_declaration_pair_in_every_layout() {
    use crate::common::corpus_manifest::{check_manifest, check_no_worse_than_base, regenerate};
    use borzoi_oracle_harness::manifest::Manifest;
    let mut total = 0;
    let mut accepted = 0;
    let mut divergent = Vec::new();
    for source in cells() {
        total += 1;
        match catch_unwind_silent(|| assert_parse_verdicts_match(&source)) {
            Ok(verdict) => {
                if verdict {
                    accepted += 1;
                }
            }
            Err(_) => {
                // Only the differential's own verdict and tree mismatches are
                // divergences to pin; any other panic (the parser's, or the
                // harness's) is a failure of its own, never hidden behind a
                // pinned cell.
                let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
                assert!(
                    message.contains("parse-verdict divergence for")
                        || message.contains("AST divergence for source"),
                    "{source:?} panicked outside the differential: {message}"
                );
                divergent.push(format!("{source:?}"));
            }
        }
    }
    let actual = Manifest::from_entries(divergent.clone()).expect("one line per cell");
    check_manifest(
        "decl_sequence_grid",
        &actual,
        &regenerate("parser_diff_decl_sequence_grid::"),
    );
    check_no_worse_than_base("decl_sequence_grid", &divergent, &[]);
    // All-accept or all-reject would pass every cell while testing nothing.
    assert!(
        accepted > 0 && accepted < total,
        "{accepted} of {total} cells accepted: the grid has degenerated"
    );
}

/// The same cells graded one level down: our lex-filter's token stream and
/// FS0058s against the stream FCS's parser reads, at the default language
/// version. A verdict that agrees with FCS on a different stream agrees by
/// accident, and is a lex-filter defect a parser change can expose; a
/// stream divergence is local and exact where the verdict is neither. Each
/// divergent cell is pinned, keyed by its source, in
/// `tests/manifests/decl_sequence_grid_streams.txt` (`stream@<i>` at the first
/// differing token, or `fs0058`). Ignored for its cost (one FCS round trip per
/// cell for the stream and one for the diagnostics); CI runs it.
#[test]
#[ignore]
fn every_declaration_pair_streams() {
    use crate::common::corpus_manifest::{check_manifest, regenerate_ignored};
    use crate::common::filtered_stream_divergence;
    use borzoi_oracle_harness::manifest::Manifest;
    let divergent: Vec<(String, String)> = cells()
        .into_iter()
        .filter_map(|source| {
            let how = filtered_stream_divergence(&source)?;
            Some((format!("{source:?}"), how))
        })
        .collect();
    let actual = Manifest::from_keyed(divergent).expect("one entry per cell");
    check_manifest(
        "decl_sequence_grid_streams",
        &actual,
        &regenerate_ignored("parser_diff_decl_sequence_grid"),
    );
}
