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
//! grammar of constructs: exhaustively, every construct with leaf holes and
//! every construct with one hole filled by another construct; and randomly,
//! trees up to [`RANDOM_DEPTH`] deep, since the defects that survive the
//! exhaustive grid are combinations of three constructs or more (a raw body's
//! trailing `;` inside a `when` guard). Each cell goes to
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
/// annotation, an assignment, a cast, an application, a prefix operator), the
/// constructs that are raw in every layout (`let … in`, `lazy`, verbose
/// `while … done`), and the loops FCS rejects without `done`.
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
    "@; ; @",
    "@;",
    "@ then @",
    "@ + @",
    "@ |> @",
    "@ :: @",
    "@ <- @",
    "@ := @",
    "@ :> int",
    "f @",
    "- @",
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
/// construct with one hole holding another leaf-filled construct, bare or in
/// parentheses.
///
/// The parenthesised filler matters because LexFilter swallows the `)` from
/// the filtered stream: a raw construct has no closing virtuals of its own, so
/// without the raw-stream closer check its continuation keyword (`else`, a
/// clause's `|`, `done`) would reach past the `)` into the enclosing
/// construct (`if a then (if b then c) else d`).
fn expressions() -> Vec<String> {
    let leafy: Vec<String> = CONSTRUCTS
        .iter()
        .map(|c| fill_nth(c, usize::MAX, LEAF))
        .collect();
    let fillers: Vec<String> = leafy
        .iter()
        .cloned()
        .chain(leafy.iter().map(|e| format!("({e})")))
        .collect();
    let mut out = leafy;
    for outer in CONSTRUCTS {
        let holes = outer.matches(HOLE).count();
        for n in 0..holes {
            for inner in &fillers {
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

/// Continuations whose left operand FCS's grammar restricts: `<-` takes a
/// `minusExpr`, `:=` a `declExpr`, and an infix a `declExpr` operand.
const SUFFIXES: &[&str] = &[" <- 1", " := 1", " |> f"];

/// Every grid expression followed by each of [`SUFFIXES`], in the
/// module-header attribute. A raw construct has no closer, so where its body
/// stops early (at a `: T` annotation, a `done`) the suffix meets the whole
/// construct as its left operand: `fun x -> 1 : int <- 1` must not become an
/// assignment to the lambda.
#[test]
fn diff_raw_keyword_grid_with_suffix() {
    sweep(
        "suffixed",
        expressions().into_iter().flat_map(|e| {
            SUFFIXES
                .iter()
                .map(move |suffix| format!("module [<A({e}{suffix})>] M\nlet x = 1\n"))
        }),
        &[],
    );
}

/// Every leaf-filled construct with one space replaced by a line break and an
/// indent, in the module-header attribute.
///
/// A line break inside the attribute list moves the lex filter: a line at
/// column 0 leaves the module header's context and opens a module-body block,
/// so a raw keyword can meet offside virtuals mid-construct, and an indented
/// line may not. Each cell is held to FCS like the rest; a parser panic fails
/// its cell.
#[test]
fn diff_raw_keyword_grid_line_breaks() {
    let cells = CONSTRUCTS.iter().flat_map(|c| {
        let e = fill_nth(c, usize::MAX, LEAF);
        let spaces: Vec<usize> = e.match_indices(' ').map(|(i, _)| i).collect();
        spaces.into_iter().flat_map(move |i| {
            let e = e.clone();
            [0, 4, 12].into_iter().map(move |indent| {
                let broken = format!("{}\n{}{}", &e[..i], " ".repeat(indent), &e[i + 1..]);
                format!("module [<A({broken})>] M\nlet x = 1\n")
            })
        })
    });
    sweep("line-break", cells, &[]);
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

/// How deep [`random_expression`] nests constructs.
const RANDOM_DEPTH: u32 = 4;

/// SplitMix64: a small deterministic generator, so a failing seed reproduces.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `true` with probability `numerator / denominator`.
    fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.next() % denominator < numerator
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

/// A random expression from [`CONSTRUCTS`], nesting at most `depth` deep: each
/// hole is a leaf or, below the depth limit, another random expression, which
/// is parenthesised one time in four (the swallowed-closer boundary).
fn random_expression(rng: &mut SplitMix64, depth: u32) -> String {
    if depth == 0 || rng.chance(1, 4) {
        return rng.pick(&["1", "x"]).to_owned();
    }
    let construct = rng.pick(CONSTRUCTS);
    let mut out = String::new();
    for (i, piece) in construct.split(HOLE).enumerate() {
        if i > 0 {
            let inner = random_expression(rng, depth - 1);
            if rng.chance(1, 4) {
                out.push_str(&format!("({inner})"));
            } else {
                out.push_str(&inner);
            }
        }
        out.push_str(piece);
    }
    out
}

/// Positions a random expression is placed in, all inside a module header's
/// attribute list so every keyword in it is raw.
const RANDOM_CONTEXTS: &[&str] = &[
    "module [<A(@)>] M\nlet x = 1\n",
    "module [<A(1, @)>] M\nlet x = 1\n",
    "module [<A(@ <- 1)>] M\nlet x = 1\n",
    "module [<A(match 0 with _ when @ -> 1)>] M\nlet x = 1\n",
    "module [<A(if @ then 1 else 2)>] M\nlet x = 1\n",
    "module [<A(@)>] M =\n  let x = 1\n",
];

/// `cases` random cells from `seed`: a random expression in a random context.
fn random_cells(seed: u64, cases: usize) -> Vec<String> {
    let mut rng = SplitMix64(seed);
    (0..cases)
        .map(|_| {
            let context = rng.pick(RANDOM_CONTEXTS);
            context.replace(HOLE, &random_expression(&mut rng, RANDOM_DEPTH))
        })
        .collect()
}

/// A fixed-seed sample of random cells, run on every test pass.
#[test]
fn diff_raw_keyword_random() {
    sweep("random", random_cells(0x5EED_0110, 2_000), &[]);
}

/// The random sweep at depth: `BORZOI_RAW_KEYWORD_SEED` picks the seed (the
/// clock by default, printed so a failure reproduces) and
/// `BORZOI_RAW_KEYWORD_CASES` the count.
#[test]
#[ignore = "deep random sweep; run explicitly when touching the raw-keyword productions"]
fn deep_diff_raw_keyword_random() {
    let seed = std::env::var("BORZOI_RAW_KEYWORD_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after the epoch")
                .as_nanos() as u64
        });
    let cases: usize = std::env::var("BORZOI_RAW_KEYWORD_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000);
    eprintln!("raw-keyword random sweep seed: {seed} (cases={cases})");
    sweep("deep random", random_cells(seed, cases), &[]);
}
