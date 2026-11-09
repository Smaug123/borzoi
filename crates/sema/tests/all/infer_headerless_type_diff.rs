//! The types a file with no `module` or `namespace` header declares, graded
//! against FCS checking the same files as one project.
//!
//! FCS puts a headerless file's contents in an implicit module named after
//! the file (`ComputeAnonModuleName`: the name without its extension, first
//! character upper-cased, split at its dots), so a type declared there is
//! `Program.Box<'a>`. Inference names it the same way when the file's name is
//! one whose every segment is ASCII letters, digits and `_`, and declines
//! otherwise.
//!
//! The grid crosses file names (plain, lower-case, dotted, with `_` and
//! digits, the last file of a multi-file project, and the declining ones: a
//! hyphen, a name a declared type shares; and a `module` header, as the
//! control) with generic unions and records at
//! arities 1 to 3 (nested generics, cases with several fields, a recursive
//! union, a field over an assembly generic, a union in a nested module
//! reached qualified and through an `open`, a `[<RequireQualifiedAccess>]`
//! union, a constrained parameter), used as values, applied, and in
//! patterns. Every node and binder inference types must be FCS's, and every
//! modelled cell of a modelled file name must commit, so the grid cannot pass
//! by declining. The ill-typed family puts the cases where FCS rejects them.

use crate::common::project_grade::{Cell, Tally, assert_clean, check};

/// Every declaration the grid's uses read, at the top of the headerless file.
const DECLS: &str = "\
type Box<'a> = Wrap of 'a | Empty
type Pair<'a, 'b> = Both of 'a * 'b | Left of 'a | Neither
type Tri<'a, 'b, 'c> = T3 of 'a * 'b * 'c | T0
type Tree<'a> = Leaf | Node of Tree<'a> * 'a * Tree<'a>
type Opt<'a> = Has of 'a option | Lacks
type Nest<'a> = Nested of Box<'a list> * Pair<'a, Box<'a>> | NoNest
type R<'a, 'b> = { A: 'a; B: 'b list; N: int }
[<RequireQualifiedAccess>]
type Q<'a> = QA of 'a | QB
type K<'a when 'a : equality> = KA of 'a | KB
module Inner =
    type In<'a> = InA of 'a | InB
";

/// Whether a use must commit.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Model {
    /// In every modelled file.
    Yes,
    /// Only under a `module` header: the resolver resolves no name through a
    /// headerless file's nested module but a type-qualified case.
    Headered,
    /// Nowhere.
    No,
}

use Model::{Headered, No, Yes};

/// `(template, model)`: one top-level binding using the declarations. `{i}`
/// is the cell's index.
const USES: &[(&str, Model)] = &[
    // Construction, generic and at ground types. A case applied to a tuple
    // whose elements stay open declines, under a header too.
    ("let g{i} x = Wrap x", Yes),
    ("let g{i} (x: int) = Wrap x", Yes),
    ("let g{i} () = Wrap \"s\"", Yes),
    ("let g{i} () = Empty", Yes),
    ("let g{i} x = Wrap (Wrap x)", Yes),
    ("let g{i} x = (Wrap x, Wrap 1)", Yes),
    ("let g{i} () = Wrap", Yes),
    ("let g{i} x y = Both (x, y)", No),
    ("let g{i} x = Left x", Yes),
    ("let g{i} () = Neither", Yes),
    ("let g{i} (x: int) (s: string) = Both (x, s)", Yes),
    ("let g{i} a b c = T3 (a, b, c)", No),
    ("let g{i} () = T3 (1, \"s\", 2.0)", Yes),
    ("let g{i} () = T0", Yes),
    ("let g{i} x = Node (Leaf, x, Leaf)", No),
    ("let g{i} (x: string) = Node (Node (Leaf, x, Leaf), x, Leaf)", Yes),
    ("let g{i} x = Has (Some x)", Yes),
    ("let g{i} () = Has None", Yes),
    ("let g{i} x = Nested (Wrap [x], Both (x, Wrap x))", No),
    ("let g{i} () = NoNest", Yes),
    ("let g{i} x = Q.QA x", Yes),
    ("let g{i} () = Q.QB", Yes),
    ("let g{i} x = Inner.InA x", Headered),
    ("let g{i} () = Inner.In.InB", Yes),
    ("let g{i} x = Box.Wrap x", Yes),
    ("let g{i} x y = Pair.Both (x, y)", No),
    // A constrained parameter's cases are not read.
    ("let g{i} x = KA x", No),
    ("let g{i} () = KB", No),
    // Record construction is not modelled; field access through a known
    // receiver is.
    ("let g{i} (x: int) = { A = x; B = [\"s\"]; N = 0 }", No),
    ("let g{i} (r: R<int, string>) = r.A", Yes),
    ("let g{i} (r: R<int, string>) = r.B", Yes),
    ("let g{i} (r: R<Box<int>, string>) = r.A", Yes),
    // Patterns over annotated scrutinees. An annotation naming a project
    // type through a qualified path or an `open` of a project module is not
    // bridged.
    (
        "let g{i} (b: Box<int>) = match b with Wrap n -> n | Empty -> 0",
        Yes,
    ),
    (
        "let g{i} (p: Pair<int, string>) = match p with Both (n, _) -> n | Left n -> n | Neither -> 0",
        Yes,
    ),
    (
        "let g{i} (p: Pair<int, string>) = match p with Both (_, s) -> s | _ -> \"\"",
        Yes,
    ),
    (
        "let g{i} (t: Tri<int, string, bool>) = match t with T3 (_, _, c) -> c | T0 -> false",
        Yes,
    ),
    (
        "let g{i} (t: Tree<int list>) = match t with Node (_, v, _) -> v | Leaf -> []",
        Yes,
    ),
    (
        "let g{i} (o: Opt<string>) = match o with Has (Some s) -> s | _ -> \"\"",
        Yes,
    ),
    (
        "let g{i} (q: Q<int>) = match q with Q.QA n -> n | Q.QB -> 0",
        Yes,
    ),
    (
        "let g{i} (b: Inner.In<int>) = match b with Inner.In.InA n -> n | Inner.In.InB -> 0",
        No,
    ),
    // Through an `open` of the nested module: everything after it.
    ("open Inner", No),
    ("let g{i} x = InA x", Headered),
    ("let g{i} () = InB", Headered),
    (
        "let g{i} (b: In<int>) = match b with InA n -> n | InB -> 0",
        No,
    ),
];

/// `(file name, header, modelled)`: the using file. A headerless file's
/// name with a hyphen is outside what inference reads, and so is one a
/// declared type shares (`Box.fs`, whose implicit module is `Box`).
const FILES: &[(&str, &str, bool)] = &[
    ("Program.fs", "", true),
    ("program.fs", "", true),
    ("Two.Parts.fs", "", true),
    ("snake_case2.fs", "", true),
    ("my-file.fs", "", false),
    ("Box.fs", "", false),
    ("Use.fs", "module M\n", true),
];

/// The using file, `header` then [`DECLS`] then every use, and its cells.
fn program(name: &str, header: &str, modelled: bool) -> (String, Vec<Cell>) {
    let mut text = format!("{header}{DECLS}");
    let mut cells = Vec::new();
    for (i, (template, model)) in USES.iter().enumerate() {
        let line = text.matches('\n').count() as u32 + 1;
        let use_text = template.replace("{i}", &i.to_string());
        text.push_str(&use_text);
        text.push('\n');
        if template.starts_with("let ") {
            let obligated = modelled
                && match model {
                    Yes => true,
                    Headered => !header.is_empty(),
                    No => false,
                };
            cells.push((line, format!("g{i}"), obligated, format!("{name}: {use_text}")));
        }
    }
    (text, cells)
}

/// How many cells of [`USES`] must commit in a file with or without a header.
fn obligated(headered: bool) -> usize {
    USES.iter()
        .filter(|(t, m)| t.starts_with("let ") && (*m == Yes || (headered && *m == Headered)))
        .count()
}

#[test]
fn headerless_file_types_agree_with_fcs_by_file_name() {
    let mut tally = Tally::default();
    let mut floor = 0;
    for (name, header, modelled) in FILES {
        let (text, cells) = program(name, header, *modelled);
        check(&[(name.to_string(), text)], &cells, &mut tally);
        if *modelled {
            floor += obligated(!header.is_empty());
        }
    }
    // The last file of a multi-file project, after a module-headed one.
    let (text, cells) = program("Main.fs", "", true);
    floor += obligated(false);
    check(
        &[
            ("Lib.fs".to_string(), "module Lib\nlet lib = 1\n".to_string()),
            ("Main.fs".to_string(), text),
        ],
        &cells,
        &mut tally,
    );
    assert_eq!(
        tally.committed_erroring, 0,
        "cells committed on lines FCS rejects: {tally:?}"
    );
    assert_clean(&tally, floor);
}

/// Uses of a headerless file's cases and fields where FCS rejects them: a
/// payload of the wrong type, a pattern contradicting the scrutinee's
/// annotation, a case of another union, a field the record lacks. Graded for
/// agreement only.
#[test]
fn ill_typed_uses_of_headerless_file_types_commit_only_what_fcs_kept() {
    let mut tally = Tally::default();
    for (i, ill) in [
        "let f0 () = (Wrap 1 : Box<string>)",
        "let f1 (b: Box<string>) = match b with Wrap n -> n + 1 | Empty -> 0",
        "let f2 (b: Box<int>) = match b with Wrap _ -> 0 | Left _ -> 1 | _ -> 2",
        "let f3 (x: string) = Both (x, 1) = Both (1, x)",
        "let f4 (r: R<int, string>) = r.Nope",
        "let f5 (r: R<int, string>) = r.A + \"s\"",
        "let f6 () = T3 (1, 2)",
        "let f7 x = Wrap x 1",
    ]
    .iter()
    .enumerate()
    {
        let name = if i % 2 == 0 { "Program.fs" } else { "Two.Parts.fs" };
        check(
            &[(name.to_string(), format!("{DECLS}{ill}\n"))],
            &[],
            &mut tally,
        );
    }
    assert_clean(&tally, 0);
}

/// A headerless file declaring a case's name again, by a route only its
/// definitions record (its nested modules' values, an active pattern's case,
/// an `exception`, a local value): FCS resolves such a name by orders the
/// resolver's history need not follow, so nothing of the name may commit
/// unless it is FCS's. Graded for agreement only.
#[test]
fn a_case_name_a_headerless_file_declares_again_agrees_with_fcs() {
    let mut tally = Tally::default();
    for src in [
        "type U = X | Y of int\nmodule A =\n    let X = 5\nopen A\nlet v = X\nlet f u = match u with X -> 0 | _ -> 1\n",
        "module A =\n    let X = 5\ntype U = X | Y of int\nopen A\nlet v = X\nlet w = Y 1\n",
        "let (|C|_|) (s: string) = Some s.Length\ntype U = C of int | D\nlet v = C 1\nlet f u = match u with C n -> n | D -> 0\n",
        "exception E of int\ntype U = E of string | F\nlet v = E \"s\"\nlet w = F\n",
        "type U = E of string | F\nexception E of int\nlet v = E 1\nlet w = F\n",
        "type U = X | Y of int\nlet f () =\n    let X = 5\n    X\nlet g () = X\nlet h () = Y 2\n",
        "[<RequireQualifiedAccess>]\ntype Q = X | Z\ntype U = X | Y of int\nlet v = X\nlet w = Q.X\n",
    ] {
        check(&[("Program.fs".to_string(), src.to_string())], &[], &mut tally);
    }
    assert_clean(&tally, 0);
}
