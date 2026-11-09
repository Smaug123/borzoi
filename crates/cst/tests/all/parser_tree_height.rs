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
//! guard. This harness checks that over the grammar's repetition forms — every
//! place the grammar lets one construct repeat, whether the repeat nests
//! (left- or right-associated, or by layout) or sits flat. Each is built far
//! past the bound, parsed on the 8 MiB stack the parser runs on in the LSP, and
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
use rowan::WalkEvent;

/// How each shape is parsed.
#[derive(Clone, Copy)]
enum Kind {
    Impl,
    Sig,
}

/// One repetition form of the grammar: `head`, then `unit` `n` times, then
/// `tail`. A construct nested by layout rather than by delimiters indents each
/// repeat `indent` columns past the one before, and the tail past the last.
struct Shape {
    name: &'static str,
    kind: Kind,
    head: &'static str,
    unit: &'static str,
    tail: &'static str,
    indent: usize,
}

impl Shape {
    fn source(&self, n: usize) -> String {
        let mut s = String::new();
        s.push_str(self.head);
        for i in 0..n {
            s.extend(std::iter::repeat_n(' ', i * self.indent));
            s.push_str(self.unit);
        }
        s.extend(std::iter::repeat_n(' ', n * self.indent));
        s.push_str(self.tail);
        s
    }

    /// The long length: [`LONG`], or [`LONG_INDENTED`] for a layout-nested
    /// shape, whose source grows quadratically.
    fn long(&self) -> usize {
        if self.indent == 0 {
            LONG
        } else {
            LONG_INDENTED
        }
    }
}

const fn impl_shape(
    name: &'static str,
    head: &'static str,
    unit: &'static str,
    tail: &'static str,
) -> Shape {
    Shape {
        name,
        kind: Kind::Impl,
        head,
        unit,
        tail,
        indent: 0,
    }
}

const fn sig_shape(
    name: &'static str,
    head: &'static str,
    unit: &'static str,
    tail: &'static str,
) -> Shape {
    Shape {
        name,
        kind: Kind::Sig,
        head,
        unit,
        tail,
        indent: 0,
    }
}

const fn indented(shape: Shape, indent: usize) -> Shape {
    Shape { indent, ..shape }
}

/// The grammar's repetition forms, by syntactic category.
const SHAPES: &[Shape] = &[
    // ---- application and postfix chains (left-nested, built in a loop) ----
    impl_shape("app", "let x = f", " a", ""),
    impl_shape("app_offside_args", "let x =\n  f\n", "    a\n", ""),
    impl_shape("high_precedence_paren_app", "let x = f", "(a)", ""),
    impl_shape("high_precedence_brack_index", "let x = a", "[0]", ""),
    impl_shape("dot_index", "let x = a", ".[0]", ""),
    impl_shape("dot_get", "let x = a", ".b", ""),
    impl_shape("method_call", "let x = a", ".M()", ""),
    impl_shape("generic_method_call", "let x = a", ".M<int>()", ""),
    impl_shape("dynamic_get", "let x = a", "?b", ""),
    impl_shape("underscore_dot_lambda", "let x = _", ".b", ""),
    impl_shape("upcast", "let x = a", " :> obj", ""),
    impl_shape("downcast", "let x = a", " :?> obj", ""),
    // ---- infix operators, one per precedence class ----
    impl_shape("op_add", "let x = a", " + a", ""),
    impl_shape("op_sub", "let x = a", " - a", ""),
    impl_shape("op_mul", "let x = a", " * a", ""),
    impl_shape("op_div", "let x = a", " / a", ""),
    impl_shape("op_mod", "let x = a", " % a", ""),
    impl_shape("op_pow", "let x = a", " ** a", ""),
    impl_shape("op_pipe", "let x = a", " |> f", ""),
    impl_shape("op_pipe_offside", "let x =\n  a\n", "  |> f\n", ""),
    impl_shape("op_add_offside", "let x =\n  a\n", "  + a\n", ""),
    impl_shape("op_compose", "let x = f", " >> f", ""),
    impl_shape("op_and", "let x = a", " && a", ""),
    impl_shape("op_or", "let x = a", " || a", ""),
    impl_shape("op_eq", "let x = a", " = a", ""),
    impl_shape("op_lt", "let x = a", " < a", ""),
    impl_shape("op_bitand", "let x = a", " &&& a", ""),
    impl_shape("op_bitor", "let x = a", " ||| a", ""),
    impl_shape("op_shl", "let x = a", " <<< a", ""),
    impl_shape("op_dollar", "let x = a", " $ a", ""),
    impl_shape("op_append", "let x = a", " @ a", ""),
    impl_shape("op_concat", "let x = a", " ^ a", ""),
    impl_shape("op_cons", "let x = a", " :: a", ""),
    impl_shape("op_assign", "let x = a", " := a", ""),
    impl_shape("op_custom", "let x = a", " >>= f", ""),
    // ---- sequences of expressions ----
    impl_shape("seq_inline", "let x = (a", "; a", ")"),
    impl_shape("seq_lines", "let f () =\n", "  a ()\n", "  ()\n"),
    impl_shape("let_chain", "let f () =\n", "  let a = 0\n", "  a\n"),
    impl_shape("use_chain", "let f () =\n", "  use a = b\n", "  a\n"),
    impl_shape("do_chain", "let f () =\n", "  do ()\n", "  ()\n"),
    impl_shape(
        "if_then_stmts",
        "let f () =\n",
        "  if a then ()\n",
        "  ()\n",
    ),
    impl_shape(
        "ce_let_bang",
        "let x =\n  async {\n",
        "    let! a = b\n",
        "    return a\n  }\n",
    ),
    impl_shape("ce_yield", "let x =\n  seq {\n", "    yield 0\n", "  }\n"),
    impl_shape("elif", "let x = if a then 0", " elif a then 0", " else 0"),
    // ---- collections and other flat repeats ----
    impl_shape("tuple", "let x = a", ", a", ""),
    impl_shape("struct_tuple", "let x = struct (a", ", a", ")"),
    impl_shape("list_inline", "let x = [a", "; a", "]"),
    impl_shape("list_lines", "let x =\n  [\n", "    a\n", "  ]\n"),
    impl_shape("array", "let x = [|a", "; a", "|]"),
    impl_shape("record", "let x = { a = 0", "; a = 0", " }"),
    impl_shape("anon_record", "let x = {| a = 0", "; a = 0", " |}"),
    impl_shape("interpolated_fills", "let x = $\"", "{a}", "\""),
    impl_shape(
        "match_arms",
        "let x =\n  match a with\n",
        "  | 0 -> 0\n",
        "",
    ),
    impl_shape("function_arms", "let f =\n  function\n", "  | 0 -> 0\n", ""),
    impl_shape(
        "try_with_arms",
        "let x =\n  try a\n  with\n",
        "  | _ -> 0\n",
        "",
    ),
    impl_shape("lambda_params", "let f = fun", " a", " -> 0"),
    impl_shape("lambda_chain", "let f = ", "fun a -> ", "0"),
    impl_shape("function_params", "let f", " a", " = 0"),
    impl_shape("attributes", "[<A", "; A", ">]\nlet x = 0\n"),
    // ---- patterns ----
    impl_shape(
        "pat_or",
        "let f x =\n  match x with\n  | 0",
        " | 0",
        " -> 0\n",
    ),
    impl_shape("pat_and", "let f (a", " & a", ") = 0"),
    impl_shape("pat_as", "let f (a", " as b", ") = 0"),
    impl_shape("pat_tuple", "let (a", ", a", ") = 0"),
    impl_shape("pat_list", "let f [a", "; a", "] = 0"),
    impl_shape("pat_cons", "let f (a", " :: a", ") = 0"),
    impl_shape("pat_long_ident", "let f (A", ".A", " a) = 0"),
    // ---- types ----
    impl_shape("type_tuple", "type T = int", " * int", ""),
    impl_shape("type_fun", "type T = int", " -> int", ""),
    impl_shape("type_postfix_app", "type T = int", " list", ""),
    impl_shape("type_long_ident", "type T = A", ".A", ""),
    impl_shape("type_generic_args", "type T = D<int", ", int", ">"),
    impl_shape(
        "type_constraints",
        "let f<'a when 'a : equality",
        " and 'a : equality",
        "> (x: 'a) = x",
    ),
    impl_shape("measure_juxtaposed", "let x = 1.0<m", " m", ">"),
    impl_shape("measure_product", "let x = 1.0<m", " * m", ">"),
    // ---- declarations ----
    impl_shape("top_lets", "", "let a = 0\n", ""),
    impl_shape("open_long_ident", "open A", ".A", "\n"),
    impl_shape("module_long_ident", "module A", ".A", "\nlet x = 0\n"),
    impl_shape("union_cases", "type T =\n", "  | A\n", ""),
    impl_shape("members", "type T() =\n", "  member _.M = 0\n", ""),
    indented(
        impl_shape("nested_modules", "", "module M =\n", "let x = 0\n"),
        2,
    ),
    indented(
        sig_shape(
            "sig_nested_modules",
            "module N\n",
            "module M =\n",
            "val x : int\n",
        ),
        2,
    ),
    sig_shape("sig_vals", "module M\n", "val a : int\n", ""),
    sig_shape("sig_type_tuple", "module M\nval a : int", " * int", "\n"),
    sig_shape(
        "sig_type_postfix_app",
        "module M\nval a : int",
        " list",
        "\n",
    ),
];

/// Twenty times [`MAX_TREE_HEIGHT`]: a shape that nests once per repeat breaches
/// the bound many times over, and unbounded its tree would overflow a 2 MiB
/// stack when dropped (a debug build overflows by 8 000 levels). The ignored
/// `deep_` twin runs a million.
const LONG: usize = 20_000;

/// The long length for a layout-nested shape, whose source is quadratic in it
/// (some 10 MB at 2-column indentation).
const LONG_INDENTED: usize = 3_000;

/// Short enough to sit far inside every bound.
const SHORT: usize = 4;

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

/// Parse and measure `src` on a thread sized like the LSP's 8 MiB main thread,
/// where it parses (the stack the parser's recursion bound is calibrated for;
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
        // A stack overflow aborts the process before the assertions can name
        // the shape, so name it first.
        eprintln!("parser_tree_height: {}", shape.name);
        let probe = parse_and_drop(shape.kind, shape.source(long(shape)));
        if !probe.roundtrip_ok {
            failures.push(format!("{}: tree is not lossless", shape.name));
        }
        if probe.height > MAX_TREE_HEIGHT {
            failures.push(format!(
                "{}: tree height {} exceeds MAX_TREE_HEIGHT ({MAX_TREE_HEIGHT})",
                shape.name, probe.height
            ));
        }
        let stopped = matches!(&probe.error_messages[..], [m] if m.contains("too deep"));
        if !probe.error_messages.is_empty() && !stopped {
            failures.push(format!(
                "{}: expected a clean parse or the one depth-limit diagnostic, got {:?}",
                shape.name, probe.error_messages
            ));
        }
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
