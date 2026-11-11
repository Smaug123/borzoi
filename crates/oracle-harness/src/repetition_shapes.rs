//! F# sources as tall as the grammar lets them grow, for checking that
//! everything which walks a parsed tree survives the tallest tree the parser
//! returns.
//!
//! The parser builds many nested nodes from a loop rather than a recursive
//! call (an application chain `f a a a`, a left-associative operator chain
//! `a + a + a`, a method chain `a.M().M()`, a postfix type application `int
//! list list`), wrapping what it has built so far under a fresh node at each
//! step, so such a chain is as deep as it is long. A recursive walk over the
//! tree recurses once per level, and a stack overflow aborts the process,
//! which `catch_unwind` cannot recover.
//!
//! [`SHAPES`] lists the grammar's repetition forms: every place the grammar
//! lets one construct repeat, whether the repeat nests (left- or
//! right-associated, or by layout) or sits flat. Built [`Shape::long`]
//! repeats long, each runs into the parser's tree-height bound; built
//! [`SHORT`] long, each parses cleanly, which is what keeps a generator from
//! passing vacuously (one the parser rejects outright stops at its first token
//! and builds a flat tree whatever its length). [`nested_in_modules`] stacks
//! layout-nested modules on top of a shape, whose levels a parse cannot count
//! while it builds the body.
//!
//! The shapes are text only, so every crate that walks the tree — the parser's
//! own height check, `borzoi-sema`'s walks, the LSP's handlers — reads the
//! same list.

/// How a shape is parsed: as an implementation file or a signature file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Impl,
    Sig,
}

/// One repetition form of the grammar: `head`, then `unit` `n` times, then
/// `tail`. A construct nested by layout rather than by delimiters indents each
/// repeat `indent` columns past the one before, and the tail past the last.
/// Each [`REPEAT_INDEX`] in `unit` is replaced by the repeat's index, so a
/// shape can declare `n` distinct names: identical repeats of a declaration
/// are one name declared `n` times, which a walk may set aside as a duplicate
/// before it does the per-declaration work a scaling test means to time.
pub struct Shape {
    pub name: &'static str,
    pub kind: Kind,
    pub head: &'static str,
    pub unit: &'static str,
    pub tail: &'static str,
    pub indent: usize,
}

/// Replaced in a [`Shape`]'s `unit` by the repeat's index (`0`, `1`, …).
pub const REPEAT_INDEX: &str = "{i}";

impl Shape {
    /// The shape with `unit` repeated `n` times.
    pub fn source(&self, n: usize) -> String {
        let mut s = String::new();
        s.push_str(self.head);
        for i in 0..n {
            s.extend(std::iter::repeat_n(' ', i * self.indent));
            if self.unit.contains(REPEAT_INDEX) {
                s.push_str(&self.unit.replace(REPEAT_INDEX, &i.to_string()));
            } else {
                s.push_str(self.unit);
            }
        }
        s.extend(std::iter::repeat_n(' ', n * self.indent));
        s.push_str(self.tail);
        s
    }

    /// The long length: [`LONG`], or [`LONG_INDENTED`] for a layout-nested
    /// shape, whose source grows quadratically.
    pub fn long(&self) -> usize {
        if self.indent == 0 {
            LONG
        } else {
            LONG_INDENTED
        }
    }

    /// Whether the shape can sit inside [`nested_in_modules`]: an
    /// implementation-file shape that neither opens with a whole-file
    /// `module` header nor nests by layout itself.
    pub fn nestable(&self) -> bool {
        self.kind == Kind::Impl && self.indent == 0 && !self.head.starts_with("module ")
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
pub const SHAPES: &[Shape] = &[
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

/// Every shape that can sit inside [`nested_in_modules`] ([`Shape::nestable`]).
pub fn nestable_shapes() -> impl Iterator<Item = &'static Shape> {
    SHAPES.iter().filter(|shape| shape.nestable())
}

/// Twenty times the parser's `MAX_TREE_HEIGHT`: a shape that nests once per
/// repeat breaches the bound many times over.
pub const LONG: usize = 20_000;

/// The long length for a layout-nested shape, whose source is quadratic in it
/// (some 10 MB at 2-column indentation).
pub const LONG_INDENTED: usize = 3_000;

/// Short enough to sit far inside every bound.
pub const SHORT: usize = 4;

/// How deep [`nested_in_modules`] nests a shape. A nested module opens its
/// node only after parsing its body, so the parse cannot count it while the
/// body grows; this many modules add several hundred such levels on top of a
/// body the parse stopped at the bound.
pub const MODULE_NEST: usize = 400;

/// The body length for a module-nested shape: past the bound for every shape
/// that nests once per repeat, and short enough that the indented body stays a
/// few megabytes.
pub const NESTED_LONG: usize = 3_000;

/// `src` as the body of [`MODULE_NEST`] nested `module M =` declarations.
pub fn nested_in_modules(src: &str) -> String {
    nested_in_modules_at(src, MODULE_NEST)
}

/// `src` as the body of `depth` nested `module M =` declarations.
pub fn nested_in_modules_at(src: &str, depth: usize) -> String {
    let mut s = String::new();
    for i in 0..depth {
        s.extend(std::iter::repeat_n(' ', 2 * i));
        s.push_str("module M =\n");
    }
    for line in src.lines() {
        s.extend(std::iter::repeat_n(' ', 2 * depth));
        s.push_str(line);
        s.push('\n');
    }
    s
}

/// The longest a [`Case`] builds a shape, alone or nested: twice the parser's
/// `MAX_TREE_HEIGHT`, which every shape that nests once per repeat breaches
/// well before. The parser's own checks build [`LONG`] repeats, to turn
/// anything it does once per repeat that is recursive or quadratic into a
/// failure; a consumer of the finished tree is after its height, which stops
/// growing at the bound, and the extra breadth would only cost time.
pub const CASE_LONG: usize = 2048;

/// Bindings for the value names and infix operators the shapes use, all at
/// `int`, which a [`Case`] puts ahead of its shape.
///
/// A consumer that gives up on a name or operator it cannot resolve gives up
/// before walking what is under it: inference declines an unresolved
/// operator before it walks either operand, so over a chain of them it never
/// recurses at all. Bound in the file itself, every operator a chain uses
/// resolves without any referenced assembly, and the chain is walked to the
/// bottom. (`::` is syntax, not an operator, and cannot be rebound.)
pub const PRELUDE: &str = "\
let a = 0
let b = 0
let f (x: int) = x
let (+) (x: int) (y: int) = x
let (-) (x: int) (y: int) = x
let ( * ) (x: int) (y: int) = x
let (/) (x: int) (y: int) = x
let (%) (x: int) (y: int) = x
let ( ** ) (x: int) (y: int) = x
let (|>) (x: int) (y: int -> int) = y x
let (>>) (x: int -> int) (y: int -> int) = fun (z: int) -> y (x z)
let (&&) (x: int) (y: int) = x
let (||) (x: int) (y: int) = x
let (=) (x: int) (y: int) = x
let (<) (x: int) (y: int) = x
let (&&&) (x: int) (y: int) = x
let (|||) (x: int) (y: int) = x
let (<<<) (x: int) (y: int) = x
let ($) (x: int) (y: int) = x
let (@) (x: int) (y: int) = x
let (^) (x: int) (y: int) = x
let (:=) (x: int) (y: int) = x
let (>>=) (x: int) (y: int -> int) = y x
let (?) (x: int) (y: string) = x
";

/// Which length of a shape a [`Case`] takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// [`Case::long`] repeats: a shape that nests stops at the bound, with the
    /// depth diagnostic, so this is the tallest tree the parser returns at all.
    Long,
    /// The longest repeat count the parser accepts without a diagnostic
    /// ([`tallest_clean_count`]): the tallest tree a consumer that declines a
    /// recovered declaration still walks.
    TallestClean,
}

/// One tall input for a consumer of the tree: a shape, alone or
/// [`nested_in_modules`], at one [`Form`].
#[derive(Clone, Copy)]
pub struct Case {
    pub shape: &'static Shape,
    pub nested: bool,
    pub form: Form,
}

impl Case {
    /// The case's name, for a failure message.
    pub fn name(&self) -> String {
        let nested = if self.nested { " (nested)" } else { "" };
        format!("{}{nested} {:?}", self.shape.name, self.form)
    }

    /// The case's source at repeat count `n`: [`Case::prelude`], then the
    /// shape, nested if the case is.
    pub fn source(&self, n: usize) -> String {
        let body = self.shape.source(n);
        let body = if self.nested {
            nested_in_modules(&body)
        } else {
            body
        };
        format!("{}{body}", self.prelude())
    }

    /// [`PRELUDE`] where it can stand: ahead of an implementation file whose
    /// shape does not open with the whole-file `module` header that must come
    /// first. A signature file declares no values, so it has none.
    pub fn prelude(&self) -> &'static str {
        if self.shape.kind == Kind::Impl && !self.shape.head.starts_with("module ") {
            PRELUDE
        } else {
            ""
        }
    }

    /// The repeat count of [`Form::Long`].
    pub fn long(&self) -> usize {
        self.shape.long().min(CASE_LONG)
    }
}

/// Every shape, alone and (where [`Shape::nestable`]) nested in modules, at
/// both forms, in a fixed order, so a child process can be handed a case by
/// its index.
pub fn cases() -> Vec<Case> {
    SHAPES
        .iter()
        .map(|shape| (shape, false))
        .chain(nestable_shapes().map(|shape| (shape, true)))
        .flat_map(|(shape, nested)| {
            [Form::Long, Form::TallestClean].map(|form| Case {
                shape,
                nested,
                form,
            })
        })
        .collect()
}

/// The longest repeat count in `SHORT..=case.long()` whose source
/// `parses_cleanly` accepts. A parse that stops at the bound stays stopped at
/// every longer count, so the count doubles from [`SHORT`] until a parse stops
/// and then bisects: the parses stay about as long as the answer, where a
/// bisection from [`Case::long`] would parse megabytes of nested source.
///
/// # Panics
///
/// Panics if the [`SHORT`] form does not parse cleanly: every shape's does,
/// and a search from a rejected start finds nothing.
pub fn tallest_clean_count(case: &Case, mut parses_cleanly: impl FnMut(&str) -> bool) -> usize {
    assert!(
        parses_cleanly(&case.source(SHORT)),
        "{}: the short form must parse cleanly",
        case.name()
    );
    let long = case.long();
    let mut clean = SHORT;
    let mut stopped = loop {
        let next = (clean * 2).min(long);
        if !parses_cleanly(&case.source(next)) {
            break next;
        }
        if next == long {
            return long;
        }
        clean = next;
    };
    while stopped - clean > 1 {
        let mid = clean + (stopped - clean) / 2;
        if parses_cleanly(&case.source(mid)) {
            clean = mid;
        } else {
            stopped = mid;
        }
    }
    clean
}
