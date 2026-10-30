//! Differential test (`parser::parse` vs FCS): statement separators in every
//! container that gathers a sequence, over a generated grid of bodies.
//!
//! FCS's `sequentialExpr` (`pars.fsy`) allows exactly one boundary between two
//! statements: `seps` — an offside `OBLOCKSEP`, a `;`, or one of each in either
//! order — or a `then` whose right-hand side is a whole `typedSequentialExpr`.
//! A run of separators is not a boundary, and neither is a `then` with nothing
//! after it. Which runs FCS accepts is a property of the token sequence, so the
//! natural test is every short sequence of statements and separators, in every
//! container that reaches the gatherer, with FCS asked for each verdict.
//!
//! Each cell is graded three ways: the parse verdict must match FCS's; when both
//! accept, the normalised trees must be equal; when both reject, the recovered
//! trees are graded by [`grade`] and every divergent cell is pinned in the
//! `seq_separators` manifest, so a recovery change in either direction moves a
//! line.

use std::collections::HashSet;
use std::io::Write as _;

use borzoi_cst::parser::parse;
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};
use tempfile::NamedTempFile;

use crate::common::corpus_manifest::check_manifest;
use crate::common::normalised_ast::{normalise_fcs_dump, normalise_parse};
use crate::common::recovery::{Relation, grade};
use crate::common::{fcs_ast_batch, fcs_parse_had_errors};

/// One token of a generated body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    /// A statement: the next of `i`, `j`, `k`, … .
    Stmt,
    Semi,
    /// `;;`, the top-level terminator: never a separator inside an expression.
    SemiSemi,
    Then,
    /// A line break onto the body's own column (an offside `OBLOCKSEP`).
    Newline,
    /// A line break onto a column right of the body's (a continuation line).
    DeepNewline,
}

const PIECES: [Piece; 6] = [
    Piece::Stmt,
    Piece::Semi,
    Piece::SemiSemi,
    Piece::Then,
    Piece::Newline,
    Piece::DeepNewline,
];

impl Piece {
    fn is_break(self) -> bool {
        matches!(self, Piece::Newline | Piece::DeepNewline)
    }
}

/// Bodies longer than the generated ones: a boundary on each side of a line
/// break, and a `then` whose right-hand side is followed by more statements.
const LONGER_BODIES: &[&[Piece]] = {
    use Piece::*;
    &[
        &[Stmt, Semi, Newline, Semi, Stmt],
        &[Stmt, Newline, Semi, Newline, Stmt],
        &[Stmt, Semi, Stmt, Semi, Stmt],
        &[Stmt, Then, Stmt, Semi, Stmt],
        &[Stmt, Then, Stmt, Then, Stmt],
        &[Stmt, Semi, Stmt, Then, Stmt],
        &[Stmt, Then, Newline, Stmt, Newline, Stmt],
        &[Stmt, Then, DeepNewline, Stmt, Newline, Stmt],
        &[Stmt, Newline, Then, Stmt, Newline, Stmt],
        &[Stmt, Then, Stmt, Newline, Stmt],
    ]
};

/// Every body of up to `MAX_PIECES` pieces holding at least one statement, no
/// two statements adjacent (that is application, not a separator question), no
/// two line breaks adjacent, and no leading line break (which moves the
/// container's own offside column rather than separating anything); then
/// [`LONGER_BODIES`].
fn bodies() -> Vec<Vec<Piece>> {
    const MAX_PIECES: usize = 4;
    let mut out = Vec::new();
    let mut frontier: Vec<Vec<Piece>> = vec![Vec::new()];
    for _ in 0..MAX_PIECES {
        let mut next = Vec::new();
        for body in &frontier {
            for &p in &PIECES {
                let last = body.last().copied();
                if last.is_none() && p.is_break() {
                    continue;
                }
                if last == Some(Piece::Stmt) && p == Piece::Stmt {
                    continue;
                }
                if last.is_some_and(Piece::is_break) && p.is_break() {
                    continue;
                }
                let mut longer = body.clone();
                longer.push(p);
                next.push(longer);
            }
        }
        out.extend(next.iter().filter(|b| b.contains(&Piece::Stmt)).cloned());
        frontier = next;
    }
    out.extend(LONGER_BODIES.iter().map(|b| b.to_vec()));
    out
}

/// The containers whose body is a sequence: each template's `{b}` is replaced
/// by the body, and the result is the right-hand side of a `let` on column 4.
/// A template's later lines carry their own indentation.
///
/// The bracket containers, `begin … end` and a computation expression reach the
/// gatherer through their own bodies; the bare `let` right-hand side, a lambda,
/// `if`/`elif`/`else` branches (`then` beside the `if`'s own `then`), a `match`
/// arm, `try`, and loop bodies through the block body; and the nested ones check
/// that the inner sequence stops where its container does.
///
/// A `let` body is the one bare sequence: it shares its block with the
/// sequence around it, so a separator past a complete boundary ends the body
/// rather than being an error (`let x = 1⏎a⏎;⏎b` is
/// `Sequential(LetOrUse(x, a), b)`). Hence the `let` forms, alone and inside a
/// closed container, and the `let!` binder.
const CONTAINERS: &[&str] = &[
    "{b}",
    "[{b}]",
    "[|{b}|]",
    "({b})",
    "begin {b} end",
    "seq {{b}}",
    "fun () -> {b}",
    "if c then {b} else k",
    "if c then k elif d then {b} else k",
    "if c then k else {b}",
    "match c with _ -> {b}",
    "try {b} with _ -> k",
    "while c do {b}",
    "[({b})]",
    "(fun () -> [{b}])",
    "let x = 1\n    {b}",
    "let x = 1 in {b}",
    "(let x = 1\n     {b})",
    "[let x = 1 in {b}]",
    "fun () -> let x = 1 in {b}",
    "if c then let x = 1 in {b} else k",
    "async { let! x = c in {b} }",
    "let x = 1\n    let w = 2\n    {b}",
];

/// Render `body` in `container` as a whole file: the container is a `let`'s
/// right-hand side, followed by another declaration so that a sequence running
/// past its container shows in the tree.
fn render(container: &str, body: &[Piece]) -> String {
    const NAMES: [&str; 5] = ["i", "j", "k", "l", "m"];
    let hole = container.find("{b}").expect("container has a body hole");
    let column = match container[..hole].rfind('\n') {
        Some(newline) => hole - newline - 1,
        None => 4 + hole,
    };
    let mut text = String::new();
    let mut names = NAMES.iter();
    for (n, &p) in body.iter().enumerate() {
        let joined = n > 0 && !body[n - 1].is_break();
        if joined && !p.is_break() {
            text.push(' ');
        }
        match p {
            Piece::Stmt => text.push_str(names.next().expect("at most five statements")),
            Piece::Semi => text.push(';'),
            Piece::SemiSemi => text.push_str(";;"),
            Piece::Then => text.push_str("then"),
            Piece::Newline => text.push_str(&format!("\n{}", " ".repeat(column))),
            Piece::DeepNewline => text.push_str(&format!("\n{}", " ".repeat(column + 4))),
        }
    }
    format!(
        "let y =\n    {}\nlet z = 1\n",
        container.replace("{b}", &text)
    )
}

/// How one cell compares with FCS.
enum Cell {
    Agree {
        accepted: bool,
    },
    /// Both reject, and the recovered trees diverge: the manifest entry.
    Divergent(String),
    /// A verdict or accepted-tree mismatch: a test failure.
    Failure(String),
}

fn grade_cell(source: &str) -> Cell {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    let json = fcs_ast_batch(tmp.path());
    let fcs_rejects = fcs_parse_had_errors(&json);
    let parse = parse(source);
    let we_reject = !parse.errors.is_empty();
    if fcs_rejects != we_reject {
        return Cell::Failure(format!(
            "{source:?}: we {}, FCS {}{}",
            if we_reject { "reject" } else { "accept" },
            if fcs_rejects { "rejects" } else { "accepts" },
            if we_reject {
                format!(" — our errors: {:?}", parse.errors)
            } else {
                String::new()
            },
        ));
    }
    if !fcs_rejects {
        let trees = catch_unwind_silent(|| (normalise_parse(&parse), normalise_fcs_dump(&json)));
        return match trees {
            Ok((ours, fcs)) if ours == fcs => Cell::Agree { accepted: true },
            Ok((ours, fcs)) => Cell::Failure(format!(
                "{source:?}: trees differ\n  ours: {ours:?}\n  fcs:  {fcs:?}"
            )),
            Err(_) => Cell::Failure(format!(
                "{source:?}: normaliser panicked: {}",
                take_silenced_panic().map_or_else(String::new, |p| p.message)
            )),
        };
    }
    match grade(&parse, &HashSet::new(), &json, source) {
        Ok(v) if v.relation == Relation::Divergent => {
            Cell::Divergent(format!("{source:?} {}", v.token()))
        }
        Ok(_) => Cell::Agree { accepted: false },
        Err(e) => Cell::Failure(format!("{source:?}: FCS record unreadable: {e}")),
    }
}

/// Every container × body cell agrees with FCS's verdict, accepted cells agree
/// on the tree, and the rejected cells' recovered-tree divergences are exactly
/// the pinned ones.
#[test]
fn seq_separator_grid_matches_fcs() {
    let bodies = bodies();
    let cells: Vec<String> = CONTAINERS
        .iter()
        .flat_map(|c| bodies.iter().map(move |b| render(c, b)))
        .collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = cells.len().div_ceil(threads);
    let graded: Vec<Cell> = std::thread::scope(|s| {
        let handles: Vec<_> = cells
            .chunks(chunk)
            .map(|part| s.spawn(move || part.iter().map(|c| grade_cell(c)).collect::<Vec<_>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("grading thread panicked"))
            .collect()
    });

    let (mut accepted, mut failures, mut divergent) = (0, Vec::new(), Vec::new());
    for cell in graded {
        match cell {
            Cell::Agree { accepted: true } => accepted += 1,
            Cell::Agree { accepted: false } => {}
            Cell::Divergent(entry) => divergent.push(entry),
            Cell::Failure(message) => failures.push(message),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cells diverge from FCS:\n{}",
        failures.len(),
        cells.len(),
        failures.join("\n"),
    );
    // All-accept or all-reject would test only one verdict.
    assert!(
        accepted > 0 && accepted < cells.len(),
        "the grid should both accept and reject, but accepted {accepted} of {}",
        cells.len(),
    );
    check_manifest(
        "seq_separators",
        &Manifest::from_entries(divergent).expect("cells are distinct"),
        "BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-cst --test all \
         parser_diff_seq_separators::",
    );
}

/// The generated bodies include each of the separator runs FCS rejects, so the
/// grid cannot silently lose the shapes it exists for.
#[test]
fn grid_covers_the_rejected_runs() {
    use Piece::*;
    let bodies = bodies();
    for shape in [
        &[Stmt, Semi, Semi, Stmt][..],
        &[Stmt, Semi, Semi],
        &[Stmt, Semi, Newline, Semi, Stmt],
        &[Stmt, Then],
        &[Stmt, Then, Semi],
        &[Stmt, Semi, Then, Stmt],
        &[Stmt, Then, Semi, Stmt],
    ] {
        assert!(bodies.iter().any(|b| b == shape), "missing {shape:?}");
    }
}
