//! Differential grid (`parser::parse` vs FCS): binding heads × right-hand
//! sides × binding contexts, the cells where FCS's parse-time
//! `SynArgNameGenerator` might be consumed.
//!
//! The parser's generator (`getSynArgNameGenerator`, on the lexbuf) has one
//! consumer: the `fun` productions (`pars.fsy`, `anonLambdaExpr`), which lower
//! a non-simple parameter to `fun _argN -> match _argN with <pat> -> …`
//! through `mkSynFunMatchLambdas`. A function-form binding head
//! (`let f 0 = …`) is lowered too, but during type checking
//! (`CheckExpressions.fs`, `PushCurriedPatternsToExpr cenv.synArgNameGenerator`),
//! with the checker's own generator, so it never touches the parse tree or the
//! parser's numbering. The generator is `.Reset()` when a module-level `let`
//! reduces, which is *after* its bindings, so the count is carried into a
//! `let` from a preceding `type` (`let g = fun 0 -> 1` after a member lambda
//! scrutinises `_arg2`).
//!
//! None of that numbering reaches the comparison: both projectors collapse a
//! generated scrutinee `_argN` to `_arg` (`canonicalise_scrutinee`), and the
//! scrutinee is the only place FCS's parse tree carries a generated name
//! outside the unprojected `SynSimplePats`. So each cell asserts two things:
//!
//! - the normalised ASTs agree and both sides parse clean, and
//! - no `_arg<digits>` survives in the agreed projection, while a canonical
//!   `_arg` scrutinee appears exactly when the right-hand side holds a
//!   non-simple `fun` parameter. The first half is what makes the index
//!   immaterial; should it ever fail, the numbering becomes part of the
//!   currency and this grid's contexts (an `and` chain, a counter carried out
//!   of a `type`) are where it must then be modelled. The second half pins
//!   that a binding head lowers nothing at parse time on either side.

use crate::common::assert_asts_match_projecting;
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};

/// The binding-name marker in [`HEADS`].
const NAME: &str = "$";

/// Binding heads. A head with [`NAME`] is function-form (usable as a member
/// head too, with the name replaced); one without is a pattern binding.
const HEADS: &[&str] = &[
    "$",
    "$ x",
    "$ 0",
    "$ _",
    "$ ()",
    "$ (a, b)",
    "$ (a, 0)",
    "$ (Some x)",
    "$ (Some x) []",
    "$ (x: int)",
    "$ X",
    "$ (x as y)",
    "$ null",
    "$ 0 (Some x) _",
    "(a, b)",
    "(a, 0)",
    "(Some a)",
    "[a]",
];

/// Right-hand sides, with whether one holds a `fun` whose parameter lowers to
/// a generated scrutinee.
const RHS: &[(&str, bool)] = &[
    ("1", false),
    ("fun x -> x", false),
    ("fun 0 -> 1", true),
    ("fun (Some y) -> y", true),
    ("fun _ 0 -> 1", true),
    ("fun (a, 0) -> a", true),
    ("fun 0 -> fun 1 -> 2", true),
    (
        "function 0 -> (fun 1 -> 2) | _ -> (fun (Some z) -> z)",
        true,
    ),
    ("match 0 with 0 -> (fun 1 -> 2) | _ -> (fun 3 -> 4)", true),
];

/// One binding context: `{H}`/`{R}` is the binding under test, `{H2}` a second
/// binding of the same head under another name. `members` marks a context
/// whose `{H}` is a member head, which only a function-form head can fill;
/// `lowers` one whose own text, outside the holes, lowers a `fun` parameter.
struct Context {
    template: &'static str,
    members: bool,
    lowers: bool,
}

const CONTEXTS: &[Context] = &[
    Context {
        template: "let {H} = {R}\n",
        members: false,
        lowers: false,
    },
    Context {
        template: "let rec {H} = {R}\nand {H2} = {R}\n",
        members: false,
        lowers: false,
    },
    Context {
        template: "let {H} = {R}\nlet {H2} = {R}\n",
        members: false,
        lowers: false,
    },
    Context {
        template: "module N =\n    let {H} = {R}\n",
        members: false,
        lowers: false,
    },
    Context {
        template: "let v =\n    let {H} = {R}\n    0\n",
        members: false,
        lowers: false,
    },
    Context {
        template: "type T() =\n    let {H} = {R}\n    member _.X = 0\n",
        members: false,
        lowers: false,
    },
    Context {
        template: "type U() =\n    member _.M = fun 0 -> 1\nlet {H} = {R}\n",
        members: false,
        lowers: true,
    },
    Context {
        template: "type T() =\n    member _.{H} = {R}\n    member _.{H2} = {R}\n",
        members: true,
        lowers: false,
    },
];

/// Every cell of the grid, with whether it lowers a `fun` parameter.
fn cells() -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for context in CONTEXTS {
        for head in HEADS {
            if context.members && !head.contains(NAME) {
                continue;
            }
            let (first, second) = if context.members {
                ("M", "N")
            } else {
                ("f", "g")
            };
            for (rhs, lowers) in RHS {
                let source = context
                    .template
                    .replace("{H2}", &head.replace(NAME, second))
                    .replace("{H}", &head.replace(NAME, first))
                    .replace("{R}", rhs);
                out.push((source, *lowers || context.lowers));
            }
        }
    }
    out
}

/// Whether `text` holds a generated name with its index, `_arg<digits>`.
fn holds_indexed_generated_name(text: &str) -> bool {
    text.match_indices("_arg")
        .any(|(i, m)| text[i + m.len()..].starts_with(|c: char| c.is_ascii_digit()))
}

/// Check one cell, panicking with the reason it fails.
fn check_cell(source: &str, lowers: bool) {
    assert!(
        !holds_indexed_generated_name(source),
        "a cell must not spell a generated name itself: {source:?}",
    );
    let agreed = assert_asts_match_projecting(source);
    let rendered = format!("{agreed:?}");
    assert!(
        !holds_indexed_generated_name(&rendered),
        "a generated `_argN` index reached the comparison for {source:?}:\n{agreed:#?}",
    );
    let has_scrutinee = rendered.contains("Ident(\"_arg\")");
    assert_eq!(
        has_scrutinee, lowers,
        "expected a generated scrutinee exactly when a `fun` parameter lowers \
         (lowers = {lowers}) for {source:?}:\n{agreed:#?}",
    );
}

#[test]
fn binding_heads_and_rhs_lambdas_agree_with_fcs() {
    let cells = cells();
    let mut failures = Vec::new();
    for (source, lowers) in &cells {
        if catch_unwind_silent(|| check_cell(source, *lowers)).is_err() {
            let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
            let first_line = message.lines().next().unwrap_or_default().to_owned();
            failures.push(format!("{source:?}: {first_line}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} binding-head cells fail:\n{}",
        failures.len(),
        cells.len(),
        failures.join("\n"),
    );
}

#[test]
fn indexed_generated_name_detector() {
    assert!(holds_indexed_generated_name("Ident(\"_arg12\")"));
    assert!(!holds_indexed_generated_name("Ident(\"_arg\")"));
    assert!(!holds_indexed_generated_name("Ident(\"_args\")"));
    assert!(!holds_indexed_generated_name(""));
}
