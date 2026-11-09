//! Every parse yields a tree shallow enough to drop on a 2 MiB stack.
//!
//! [`parser_depth_limit`](super::parser_depth_limit) bounds the parser's own
//! *recursion*. That bound does not bound the *tree*: the parser builds many
//! nested nodes from a loop rather than a recursive call (an application chain
//! `f a a a`, a left-associative operator chain `a + a + a`, a method chain
//! `a.M().M()`, a postfix type application `int list list`), wrapping what it
//! has built so far under a fresh node at each step. Such a chain is as deep as
//! it is long, and everything that walks a rowan tree recursively — rowan's own
//! green-node `Drop` first of all — recurses once per level. A stack overflow
//! aborts the process; `catch_unwind` cannot catch it.
//!
//! So the bound is drawn on the tree the builder produces, not on any one
//! production: no parse returns a tree taller than [`MAX_TREE_HEIGHT`]; the
//! parse stops short of it with the same single diagnostic as the recursion
//! guard. This harness checks that over the grammar's repetition forms
//! ([`repetition_shapes`](borzoi_oracle_harness::repetition_shapes)) — every
//! place the grammar lets one construct repeat, whether the repeat nests
//! (left- or right-associated, or by layout) or sits flat. Each is built far
//! past the bound, parsed on an 8 MiB stack (the one its recursion bound is
//! calibrated for, and a quarter of the stack the LSP parses on), and
//! its tree dropped on a thread with an explicit 2 MiB stack (a Rust thread's
//! default, and a tokio worker's). A long input exercises more than the tree:
//! it also turns any recursion the parser or the lex filter does once per
//! repeat into an abort, and anything quadratic in the repeat count into a
//! test that does not finish.
//!
//! Each shape is also parsed short, where it must parse cleanly: a generator
//! the parser rejects outright would stop at its first token and build a flat
//! tree whatever its length, passing vacuously.

use borzoi_cst::parser::{MAX_TREE_HEIGHT, Parse, parse, parse_sig};
use borzoi_cst::syntax::SyntaxNode;
use borzoi_oracle_harness::repetition_shapes::{
    Kind, LONG_INDENTED, NESTED_LONG, SHAPES, SHORT, Shape, nestable_shapes, nested_in_modules,
};
use rowan::WalkEvent;

/// The `Send`-able facts about one parse (a `Parse` holds rowan nodes, which
/// are not `Send`).
struct Probe {
    height: usize,
    roundtrip_ok: bool,
    error_messages: Vec<String>,
}

/// Node height of `root`: the longest root-to-leaf chain of nodes, the root
/// counting as 1. Iterative, so measuring a tall tree cannot overflow.
fn tree_height(root: &SyntaxNode) -> usize {
    let mut depth = 0usize;
    let mut height = 0usize;
    for event in root.preorder() {
        match event {
            WalkEvent::Enter(_) => {
                depth += 1;
                height = height.max(depth);
            }
            WalkEvent::Leave(_) => depth -= 1,
        }
    }
    height
}

fn parse_kind(kind: Kind, src: &str) -> Parse {
    match kind {
        Kind::Impl => parse(src),
        Kind::Sig => parse_sig(src),
    }
}

/// Parse and measure `src` on an 8 MiB thread
/// (the stack the parser's recursion bound is calibrated for;
/// see `parser_depth_limit`), then drop the tree on a thread with a 2 MiB
/// stack. The drop is the point: rowan frees a green tree recursively, so a
/// tree too tall for the stack aborts the process there.
fn parse_and_drop(kind: Kind, src: String) -> Probe {
    let (probe, green) = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let parsed = parse_kind(kind, &src);
            let probe = Probe {
                height: tree_height(&parsed.root),
                roundtrip_ok: parsed.root.text() == src.as_str(),
                error_messages: parsed.errors.iter().map(|e| e.message.clone()).collect(),
            };
            // The green tree is `Send`; the red tree over it is not. Dropping
            // the red tree here leaves `green` the last owner.
            let green = parsed.root.green().to_owned();
            (probe, green)
        })
        .expect("spawn parse thread")
        .join()
        .expect("parse thread panicked");
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || drop(green))
        .expect("spawn drop thread")
        .join()
        .expect("drop thread panicked");
    probe
}

#[test]
fn every_shape_parses_cleanly_when_short() {
    let mut failures = Vec::new();
    for shape in SHAPES {
        let probe = parse_and_drop(shape.kind, shape.source(SHORT));
        if !probe.roundtrip_ok || !probe.error_messages.is_empty() {
            failures.push(format!("{}: {:?}", shape.name, probe.error_messages));
        }
    }
    assert!(
        failures.is_empty(),
        "these generators do not produce input the parser accepts, so their long \
         form proves nothing:\n{}",
        failures.join("\n")
    );
}

/// Parse every shape `long(shape)` repeats long, and check that its tree is
/// lossless, no taller than [`MAX_TREE_HEIGHT`], and either clean or stopped
/// with the single depth-limit diagnostic.
fn check_every_shape_long(long: impl Fn(&Shape) -> usize) {
    let mut failures = Vec::new();
    for shape in SHAPES {
        check_long(
            shape.name,
            shape.kind,
            shape.source(long(shape)),
            &mut failures,
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Parse and drop one long input (see [`parse_and_drop`]), recording in
/// `failures` how it breaks the contract, if it does.
fn check_long(name: &str, kind: Kind, src: String, failures: &mut Vec<String>) {
    // A stack overflow aborts the process before the assertions can name the
    // input, so name it first.
    eprintln!("parser_tree_height: {name}");
    let probe = parse_and_drop(kind, src);
    if !probe.roundtrip_ok {
        failures.push(format!("{name}: tree is not lossless"));
    }
    if probe.height > MAX_TREE_HEIGHT {
        failures.push(format!(
            "{name}: tree height {} exceeds MAX_TREE_HEIGHT ({MAX_TREE_HEIGHT})",
            probe.height
        ));
    }
    let stopped = matches!(&probe.error_messages[..], [m] if m.contains("too deep"));
    if !probe.error_messages.is_empty() && !stopped {
        failures.push(format!(
            "{name}: expected a clean parse or the one depth-limit diagnostic, got {:?}",
            probe.error_messages
        ));
    }
    if stopped && probe.height < STOPPED_TREE_MIN_HEIGHT {
        failures.push(format!(
            "{name}: stopped with a tree only {} deep, so the stop came early, not at \
             the bound — a loop that wraps a node per repeat may not be checking it \
             (`depth_limit_latched`), leaving only the redo-from-a-lower-stop fallback",
            probe.height
        ));
    }
}

/// A parse only stops because something grew tall or deep, so its tree is at
/// least this tall: the shallowest stop, the recursion guard's on a chain that
/// costs it two levels per repeat, leaves about 260. A tree much shorter means
/// the parse gave up near the start instead.
const STOPPED_TREE_MIN_HEIGHT: usize = 200;

/// Every shape, nested inside
/// [`MODULE_NEST`](borzoi_oracle_harness::repetition_shapes::MODULE_NEST)
/// modules: the stop the parse chooses while it builds the body cannot see the
/// module nodes still to come, so this checks the bound holds for the finished
/// tree, not just for the part of it the parse could measure. Each must parse
/// cleanly when short, as above, so the nesting is not vacuous.
#[test]
fn every_shape_nested_in_modules_stays_within_the_bound() {
    let mut failures = Vec::new();
    for shape in nestable_shapes() {
        let short = parse_and_drop(shape.kind, nested_in_modules(&shape.source(SHORT)));
        if !short.roundtrip_ok || !short.error_messages.is_empty() {
            failures.push(format!(
                "{} (nested, short): {:?}",
                shape.name, short.error_messages
            ));
        }
        check_long(
            &format!("{} (nested)", shape.name),
            shape.kind,
            nested_in_modules(&shape.source(NESTED_LONG)),
            &mut failures,
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_shape_stays_within_the_tree_height_bound_when_long() {
    check_every_shape_long(Shape::long);
}

/// [`every_shape_stays_within_the_tree_height_bound_when_long`] at a million
/// repeats (layout-nested shapes excepted, whose source is quadratic).
#[test]
#[ignore = "several minutes in a debug build"]
fn deep_every_shape_stays_within_the_tree_height_bound() {
    check_every_shape_long(|shape| {
        if shape.indent == 0 {
            1_000_000
        } else {
            LONG_INDENTED
        }
    });
}

/// Prints each shape's tree height at a few lengths (`!` marking a parse with
/// errors), for calibrating [`MAX_TREE_HEIGHT`] against the shapes.
#[test]
#[ignore = "calibration report"]
fn report_shape_heights() {
    for shape in SHAPES {
        let row: Vec<String> = [10, 100, 1000]
            .into_iter()
            .map(|n| {
                let p = parse_and_drop(shape.kind, shape.source(n));
                let mark = if p.error_messages.is_empty() { "" } else { "!" };
                format!("{n}:{}{mark}", p.height)
            })
            .collect();
        println!("{:<28} {}", shape.name, row.join("  "));
    }
}
