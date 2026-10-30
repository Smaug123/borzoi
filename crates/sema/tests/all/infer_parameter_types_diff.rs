//! A function parameter's own type, against FCS's `binder-types` oracle.
//!
//! A parameter's type is published where it is known for certain: its
//! annotation, which F# keeps on the binder even where the body is rejected,
//! or its place in a function type inference publishes, whose domains are
//! the parameters' types.
//!
//! Every commit is graded against FCS by `infer_operators_diff::check_in`,
//! and each case names the parameters it must commit, so a decline cannot
//! pass vacuously. A generic parameter stays silent: FCS renders it renumbered
//! within its own type (`'a` for the second of `'a -> 'b -> 'a`), while hover
//! would show it as the function's (`'b`), and one published type cannot be
//! both.

use crate::infer_operators_diff::{Defs, check_in};

/// The functions the cases apply, and the cases: each a binding, and the
/// parameters of it that must commit.
const HEADER: &str = "module Params\n\
                      let inc (x: int) = x\n\
                      let incs (s: string) = 1\n";

/// One case: a binding, and its parameters that must commit.
const CASES: &[(&str, &[&str])] = &[
    // Annotated, of every annotation shape the bridge reads.
    ("let f1 (a: int) (b: string) = a", &["a", "b"]),
    ("let f2 (a: int * string) (b: int -> int) = b", &["a", "b"]),
    ("let f3 (a: int[]) (b: obj) = b", &["a", "b"]),
    // Annotated, in a body inference does not model: the annotation alone.
    ("let f4 (a: int) = match a with _ -> 1", &["a"]),
    ("let f5 (a: int) b = match b with _ -> a", &["a"]),
    // Unannotated, fixed by an argument check in a complete binding.
    ("let f6 a = inc a", &["a"]),
    ("let f7 a b = (inc a, incs b)", &["a", "b"]),
    // A condition.
    ("let f8 c = if c then 1 else 2", &["c"]),
    // A tuple parameter's elements.
    ("let f9 (a, b) = (inc a, incs b)", &["a", "b"]),
    ("let f10 (a: int, b) = incs b", &["a", "b"]),
    // A member access on an annotated parameter.
    ("let f11 (s: string) = s.Length", &["s"]),
    // A statically resolved operator over a known operand.
    ("let f12 (a: int) b = a + inc b", &["a", "b"]),
    // Generic: silent, since FCS renumbers a parameter's type on its own.
    ("let g1 x y = x", &[]),
    ("let g2 (a: int) b = (a, b)", &["a"]),
    ("let g3 (a, b) = (inc a, b)", &["a"]),
    // Open where it is applied: weak resolution, which inference does not
    // model, decides it.
    ("let o1 a = a + 1", &[]),
    // A parameter used before anything fixes it, so FCS rejects the member
    // access; the binding's type is not FCS's to agree with.
    ("let o2 s = (s.Length, incs s)", &[]),
];

#[test]
fn every_parameter_whose_type_is_known_commits_it() {
    let mut src = String::from(HEADER);
    let mut wanted: Vec<(usize, usize, String)> = Vec::new();
    for (case, params) in CASES {
        let line_start = src.len();
        src.push_str(case);
        src.push('\n');
        let head_end = case.find('=').expect("a binding");
        for p in *params {
            let at = case[..head_end]
                .match_indices(p)
                .find(|(i, _)| {
                    let before = case[..*i].chars().next_back();
                    let after = case[i + p.len()..].chars().next();
                    !before.is_some_and(|c| c.is_alphanumeric())
                        && !after.is_some_and(|c| c.is_alphanumeric())
                })
                .map(|(i, _)| i)
                .unwrap_or_else(|| panic!("`{p}` in the head of `{case}`"));
            wanted.push((
                line_start + at,
                line_start + at + p.len(),
                format!("`{p}` of `{case}`"),
            ));
        }
    }
    let c = check_in(Defs::InFile, "params", &src, &[], false)
        .unwrap_or_else(|| panic!("FCS checks the file\n{src}"));
    let committed: std::collections::HashSet<(usize, usize)> = c
        .commits
        .iter()
        .filter(|(_, _, kind, _)| *kind == "binder")
        .map(|(s, e, _, _)| (*s, *e))
        .collect();
    let missed: Vec<&str> = wanted
        .iter()
        .filter(|(s, e, _)| !committed.contains(&(*s, *e)))
        .map(|(_, _, what)| what.as_str())
        .collect();
    assert!(
        missed.is_empty(),
        "{} parameter(s) whose type is known did not commit:\n{}",
        missed.len(),
        missed.join("\n")
    );
}
