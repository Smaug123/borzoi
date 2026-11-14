//! Differential test (`parser::parse` vs FCS): members that follow a type's
//! representation without a `with`, over every representation form, layout and
//! member item.
//!
//! FCS's `tyconDefnRhsBlock: OBLOCKBEGIN tyconDefnRhs opt_OBLOCKSEP
//! classDefnMembers …` (`pars.fsy:1730`) admits them, but which representations
//! take them depends on the layout: a member *aligned* with the representation
//! arrives behind an `OBLOCKSEP`, and a representation that could still be a
//! bar-less union's first case (`A`, `A of int`) then rejects it, while a member
//! indented *deeper* arrives with no separator and every representation but an
//! inline-IL one takes it. A table written per representation in one layout
//! misses the other, so this grid crosses the three and grades each cell with
//! [`assert_parse_verdicts_match`]: a cell we wrongly reject, wrongly accept, or
//! accept with a divergent tree fails. Nothing here hard-codes which cells FCS
//! accepts.
//!
//! The cells [`known`] names diverge for reasons outside this grid's subject,
//! each with its issue. They must still diverge, so the predicate cannot outlive
//! the defect it records.

use crate::common::assert_parse_verdicts_match;
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};

/// The shape of a representation, as far as [`known`] needs to tell them apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Record,
    Union,
    Enum,
    /// A representation that could still be a bar-less union's first case.
    LoneCase,
    Abbrev,
    KindMarked,
    Delegate,
    InlineIl,
    ObjectModel,
}

/// Representations, one source line per element, each written at column 0 and
/// indented by the layout, and whether they fit on the `=` line.
const REPRS: &[(&[&str], bool, Shape)] = &[
    (&["{ X: int }"], true, Shape::Record),
    (&["{ X: int", "  Y: int }"], false, Shape::Record),
    (&["| A"], true, Shape::Union),
    (&["| A", "| B of int"], false, Shape::Union),
    (&["A", "| B"], false, Shape::Union),
    (&["A"], true, Shape::LoneCase),
    (&["A of int"], true, Shape::LoneCase),
    (&["int"], true, Shape::LoneCase),
    (&["``A``"], true, Shape::LoneCase),
    (&["``A`` of int"], true, Shape::LoneCase),
    (&["'a"], true, Shape::Abbrev),
    (&["A = 1"], true, Shape::Enum),
    (&["| A = 1", "| B = 2"], false, Shape::Enum),
    (&["int list"], true, Shape::Abbrev),
    (&["int -> int"], true, Shape::Abbrev),
    (&["int * string"], true, Shape::Abbrev),
    (&["System.String"], true, Shape::Abbrev),
    (&["class", "end"], false, Shape::KindMarked),
    (&["struct", "end"], false, Shape::KindMarked),
    (&["interface", "end"], false, Shape::KindMarked),
    (&["class end"], true, Shape::KindMarked),
    (
        &["struct", "    val X: int", "end"],
        false,
        Shape::KindMarked,
    ),
    (&["delegate of int -> int"], true, Shape::Delegate),
    (&["( # \"int32\" # )"], true, Shape::InlineIl),
    (&["member _.Y = 2"], false, Shape::ObjectModel),
];

/// The member item kinds [`known`] needs to tell apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Item {
    Member,
    Static,
    TwoMembers,
    Let,
    Do,
    Override,
    Interface,
}

/// Member items, one source line per element, the first at the item's column
/// and the rest indented relative to it.
const ITEMS: &[(&[&str], Item)] = &[
    (&["member _.M = 1"], Item::Member),
    (&["static member S = 1"], Item::Static),
    (&["member _.M = 1", "member _.N = 2"], Item::TwoMembers),
    (&["let x = 1"], Item::Let),
    (&["do ()"], Item::Do),
    (&["override _.ToString() = \"\""], Item::Override),
    (
        &[
            "interface System.IDisposable with",
            "    member _.Dispose() = ()",
        ],
        Item::Interface,
    ),
];

/// The type's header, and the column a representation written on its `=` line
/// starts at. A column-0 attributed header puts the name at column 0, where
/// FCS's `=` can open no body block, and a member after the representation is
/// then never a bare member. Under it the grid generates only the member
/// items after a non-object-model representation: its own divergences for an
/// object model and a `let` (FCS takes an indented `let` as a module-level
/// declaration) are recorded on #647.
const HEADERS: &[(&str, usize)] = &[("type T =", 9), ("type [<A>]\nT =", 4)];

/// Where the representation and the item sit: the representation's column on
/// its own line under the header (`None`: on the `=` line, at its [`HEADERS`]
/// column), and the item's column (`None`: on the representation's last line,
/// after it, with any further item lines at the column it starts at). They
/// cover an item aligned with the representation, deeper than it, shallower,
/// and on the same line.
const LAYOUTS: &[(Option<usize>, Option<usize>)] = &[
    (Some(4), Some(4)),
    (Some(4), Some(5)),
    (Some(4), Some(8)),
    (Some(4), Some(2)),
    (Some(4), None),
    (None, Some(4)),
    (None, Some(9)),
    (None, None),
];

/// A following module-level declaration, which pins where the type's block
/// closes.
const TRAILERS: &[&str] = &["", "let z = 0\n"];

struct Cell {
    source: String,
    shape: Shape,
    item: Item,
    repr_col: usize,
    item_col: usize,
    trailer: bool,
    /// Whether the item continues the representation's last line.
    same_line: bool,
}

/// The issue a cell's divergence is recorded under, if it is one this grid
/// does not grade.
fn known(cell: &Cell) -> Option<&'static str> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let order = cell.item_col.cmp(&cell.repr_col);
    match (cell.item, order) {
        // A `let` undented from the type body, at the end of the file: FCS
        // takes it as a module-level declaration.
        (Item::Let, Less) if !cell.trailer && cell.shape != Shape::InlineIl => Some("#558"),
        // A class `let` indented deeper than (or on the line of) a union,
        // enum, abbreviation or object model, before a module-level
        // declaration, and one aligned with a lone case or an inline-IL
        // abbreviation. On an object model's line the `let` agrees.
        (Item::Let, Greater)
            if cell.trailer
                && matches!(
                    cell.shape,
                    Shape::Union | Shape::Enum | Shape::LoneCase | Shape::Abbrev
                )
                || cell.trailer && cell.shape == Shape::ObjectModel && !cell.same_line =>
        {
            Some("#647")
        }
        (Item::Let, Equal) if matches!(cell.shape, Shape::LoneCase | Shape::InlineIl) => {
            Some("#647")
        }
        // An item indented deeper than (or on the line of) an object model's
        // last member, before a module-level declaration. On its line a `do`
        // agrees.
        (Item::Member | Item::Override, Greater)
            if cell.trailer && cell.shape == Shape::ObjectModel =>
        {
            Some("#647")
        }
        (Item::Do, Greater)
            if cell.trailer && cell.shape == Shape::ObjectModel && !cell.same_line =>
        {
            Some("#647")
        }
        _ => None,
    }
}

fn indent(col: usize, line: &str) -> String {
    format!("{}{line}\n", " ".repeat(col))
}

fn cells() -> Vec<Cell> {
    let mut out = Vec::new();
    for &(header, inline_col) in HEADERS {
        let blockless = header.starts_with("type [<");
        for &(repr, fits_inline, shape) in REPRS {
            for &(repr_col, item_col) in LAYOUTS {
                if repr_col.is_none() && !fits_inline {
                    continue;
                }
                for &(item_lines, item) in ITEMS {
                    if blockless && (shape == Shape::ObjectModel || item == Item::Let) {
                        continue;
                    }
                    for trailer in TRAILERS {
                        let mut source = String::from(header);
                        match repr_col {
                            None => source.push_str(&format!(" {}\n", repr[0])),
                            Some(col) => {
                                source.push('\n');
                                for line in repr {
                                    source.push_str(&indent(col, line));
                                }
                            }
                        }
                        let same_line = item_col.is_none();
                        let item_col = match item_col {
                            Some(col) => col,
                            None => {
                                // Continue the representation's last line.
                                source.pop();
                                let line_start = source.rfind('\n').map_or(0, |n| n + 1);
                                source.push(' ');
                                source.len() - line_start
                            }
                        };
                        for (i, line) in item_lines.iter().enumerate() {
                            if i == 0 && same_line {
                                source.push_str(&format!("{line}\n"));
                            } else {
                                source.push_str(&indent(item_col, line));
                            }
                        }
                        source.push_str(trailer);
                        out.push(Cell {
                            source,
                            shape,
                            item,
                            repr_col: repr_col.unwrap_or(inline_col),
                            item_col,
                            trailer: !trailer.is_empty(),
                            same_line,
                        });
                    }
                }
            }
        }
    }
    out
}

#[test]
fn bare_members_after_every_representation() {
    let mut total = 0;
    let mut accepted = 0;
    let mut failures = Vec::new();
    let mut known_now_agreeing = Vec::new();
    for cell in cells() {
        total += 1;
        let known = known(&cell);
        match catch_unwind_silent(|| assert_parse_verdicts_match(&cell.source)) {
            Ok(verdict) => {
                if verdict {
                    accepted += 1;
                }
                if let Some(issue) = known {
                    known_now_agreeing.push(format!("{issue}: {:?}", cell.source));
                }
            }
            Err(_) => {
                let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
                if known.is_none() {
                    let first_line = message.lines().next().unwrap_or_default().to_owned();
                    failures.push(first_line);
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} cells diverge from FCS:\n{}",
        failures.len(),
        failures.join("\n"),
    );
    assert!(
        known_now_agreeing.is_empty(),
        "these cells are recorded as known divergences but now agree with FCS; \
         narrow `known`:\n{}",
        known_now_agreeing.join("\n"),
    );
    // All-accept or all-reject would pass every cell while testing nothing.
    assert!(
        accepted > 0 && accepted < total,
        "{accepted} of {total} cells accepted: the grid has degenerated"
    );
}
