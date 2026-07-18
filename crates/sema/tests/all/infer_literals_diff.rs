//! Differential test for [`borzoi_sema::infer_file`] against FCS's typed
//! tree (the `fcs-dump types` oracle): Stage-3.1 literal typing and Stage-3.2b-1
//! value-reference propagation.
//!
//! The headline property is **soundness** (D5): *for every type we infer, FCS
//! agrees at that exact range.* We iterate **our** inferred types — not FCS's
//! nodes — because the danger this layer must never realise is saying something
//! *wrong*, and that can only happen at a range where we speak. Saying nothing
//! is always allowed (Deferred).
//!
//! Stage 3.1 types a literal only in a *soundness-safe position*: the immediate
//! RHS of an unannotated, simple-name `let` binding, where no expected type can
//! retarget it. So the tests come in two halves:
//!   * **sound positions** (`let x = <lit>`) — every literal kind must be typed
//!     and agree with FCS, including the unsuffixed `42` / `"hi"` that a
//!     kind-based rule would have to defer;
//!   * **unsound positions** (argument, annotated, typed-pattern, measure,
//!     collection, format) — must stay silent, because there an expected type
//!     can change the literal's type (`let x: int64 = 42`, `printfn "%d"`,
//!     `1.0<kg>`, `op_Implicit` targets).
//!
//! These call FCS once per snippet (like `resolve_diff`), so the kind sweep is
//! bundled into a single binding-per-line snippet to amortise the type-check.

use crate::common::{invoke_fcs_dump, parse_fcs_types, temp_fs_file};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, InferredFile, ProjectItems, Ty, infer_file, resolve_file};
use proptest::prelude::*;

/// Resolve `file` (single-file: empty project + no referenced assemblies, which
/// is all within-file value-reference typing needs) and infer it.
fn infer(file: &ImplFile) -> InferredFile {
    let env = AssemblyEnv::default();
    let resolved = resolve_file(file, &ProjectItems::default(), &env);
    infer_file(file, &resolved, &env)
}

/// Parse `source` (asserting it is in our subset) and infer it.
fn infer_src(source: &str) -> InferredFile {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors (outside the subset?): {source:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    infer(&file)
}

/// Infer `source`, run the FCS `types` oracle over it, and assert the D5
/// soundness property for every type we produced. Returns how many we checked.
fn assert_sound(source: &str) -> usize {
    let inferred = infer_src(source);

    let path = temp_fs_file("infer_lit", source);
    let json = invoke_fcs_dump("types", &path);
    let _ = std::fs::remove_file(&path);
    let fcs = parse_fcs_types(&json, source);

    let mut checked = 0usize;
    for (range, ty) in inferred.types() {
        let key = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        let fcs_ty = fcs.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{}` at {key:?} but FCS reports no node there in {source:?}",
                ty.render()
            )
        });
        assert_eq!(
            &ty.render(),
            fcs_ty,
            "type mismatch at {key:?} in {source:?}: ours=`{}`, FCS=`{fcs_ty}`",
            ty.render()
        );
        checked += 1;
    }
    checked
}

/// Every literal kind we type, one per unannotated `let` binding (a sound
/// position). Each must be typed and agree with FCS — including the unsuffixed
/// `int`/`float`/`string` that are only sound *because* of the position.
#[test]
fn bare_rhs_literals_match_fcs() {
    let source = "\
module Lit
let s1 = \"plain\"
let s2 = @\"verbatim\"
let s3 = \"\"\"triple\"\"\"
let ch = 'c'
let bo = true
let by1 = \"bytes\"B
let by2 = @\"bytes\"B
let i32 = 42
let sb = 1y
let bt = 2uy
let i16 = 3s
let u16 = 4us
let u32 = 5u
let i64 = 6L
let u64 = 7UL
let np = 8n
let unp = 9un
let f64 = 1.5
let f32 = 2.0f
let dec = 3.5m
";
    let checked = assert_sound(source);
    // 20 literals above, each the RHS of a sound binding → one typed node each.
    assert_eq!(checked, 20, "expected every bare-RHS literal to be typed");
}

/// A `let mutable` binding is still an unannotated simple-name binding, so its
/// literal is typed and agrees with FCS — the modifier does not change the sound
/// position. (`[<Literal>]` constants and class-local `let`s are *not* yet typed
/// — an uppercase `K` parses as a constructor-shaped pattern, and class-local
/// bindings are not a `LET_DECL` — but staying silent there is sound; widening
/// coverage to them is future work.)
#[test]
fn mutable_binding_is_typed() {
    let source = "module Edge\nlet mutable m = 7\n";
    assert_eq!(
        assert_sound(source),
        1,
        "the mutable binding's `7` should type"
    );
}

/// A value *use* inherits its binder's type and agrees with FCS — including down
/// a chain of value bindings (`z = y`, `y = x`, `x = 42`). This is the first
/// hover win on a use, the point of Stage 3.2b-1.
#[test]
fn value_uses_match_fcs() {
    let source = "\
module V
let x = 42
let y = x
let z = y
let s = \"hi\"
let s2 = s
";
    // 2 literal RHS (`42`, `\"hi\"`) + 3 value uses (`x` in `let y`, `y` in
    // `let z`, `s` in `let s2`) = 5 typed nodes, every one agreeing with FCS.
    assert_eq!(assert_sound(source), 5);
}

/// A use of a binder whose type we never determine — here a function value —
/// stays Deferred rather than guessing, and `assert_sound` confirms we emit
/// nothing wrong. Pins that value-reference typing never over-claims.
#[test]
fn use_of_untyped_binding_is_deferred() {
    let source = "module N\nlet f x = x\nlet g = f\n";
    assert!(
        infer_src(source).is_empty(),
        "a use of an untyped (function) binding must stay Deferred"
    );
    assert_eq!(assert_sound(source), 0);
}

/// A value use in a **coercion context** must be deferred: `s : string`, but the
/// annotated binding `o : obj` makes F# insert a subsumption coercion, so FCS
/// reports `System.Object` at the use `s` — not its binder's `System.String`.
/// Typing the use as the binder's type would be a D5 violation. A use is only
/// sound to type where no expected type reaches it (a bare, unannotated `let`
/// RHS); annotated/argument/upcast positions defer.
#[test]
fn value_use_in_coercion_context_is_deferred() {
    let source = "module C\nlet s = \"hi\"\nlet o : obj = s\n";
    // Only the literal RHS `\"hi\"` (string) is typed; the coerced use `s` is
    // deferred. `assert_sound` confirms FCS agrees on everything we *do* emit
    // (and would catch us wrongly emitting `string` where FCS says `obj`).
    assert_eq!(
        assert_sound(source),
        1,
        "only the literal RHS is typed; the coerced use `s` must be deferred"
    );
}

/// Parentheses are transparent (3.2b-2): `let y = (x)` types the use `x` exactly
/// as the unparenthesised form would, and FCS reports no node at the parens
/// themselves — so the only typed nodes are the literal `42` and the use `x`.
#[test]
fn parenthesised_value_matches_fcs() {
    let source = "module P\nlet x = 42\nlet y = (x)\n";
    assert_eq!(
        assert_sound(source),
        2,
        "the literal `42` and the parenthesised use `x` type; the parens add no node"
    );
}

/// A tuple binding types as `Ty::Tuple`, and so do its elements, all agreeing
/// with FCS (3.2b-2).
#[test]
fn tuple_binding_matches_fcs() {
    let source = "module T\nlet p = (1, \"hi\")\n";
    let inferred = infer_src(source);
    let renders: Vec<String> = inferred.types().values().map(Ty::render).collect();
    assert!(
        renders.contains(&"System.Int32 * System.String".to_string()),
        "expected the tuple type among {renders:?}"
    );
    // The tuple node `(1, \"hi\")` plus its two element literals = 3 typed nodes,
    // every one agreeing with FCS.
    assert_eq!(assert_sound(source), 3);
}

/// A nested tuple renders the inner tuple parenthesised, matching FCS's canonical
/// (`System.Int32 * (System.Double * System.String)`) — the join stays
/// unambiguous and the two sides agree byte-for-byte.
#[test]
fn nested_tuple_matches_fcs() {
    let source = "module T\nlet p = (1, (2.0, \"x\"))\n";
    let inferred = infer_src(source);
    let renders: Vec<String> = inferred.types().values().map(Ty::render).collect();
    assert!(
        renders.contains(&"System.Int32 * (System.Double * System.String)".to_string()),
        "expected the nested tuple type among {renders:?}"
    );
    // Outer tuple + inner tuple + the three literals (`1`, `2.0`, `\"x\"`) = 5.
    assert_eq!(assert_sound(source), 5);
}

/// A tuple of value *uses* types from the binders, and the tuple and each use
/// agree with FCS — value-reference propagation composing with tuples.
#[test]
fn tuple_of_value_uses_matches_fcs() {
    let source = "module T\nlet a = 1\nlet b = \"hi\"\nlet p = (a, b)\n";
    let inferred = infer_src(source);
    let renders: Vec<String> = inferred.types().values().map(Ty::render).collect();
    assert!(
        renders.contains(&"System.Int32 * System.String".to_string()),
        "expected the tuple of uses among {renders:?}"
    );
    // `1`, `\"hi\"` (literal RHSs) + the uses `a`, `b` + the tuple node = 5.
    assert_eq!(assert_sound(source), 5);
}

/// 3.2c-1: `if`/`then`/`else` types as the result (then-branch) type and agrees
/// with FCS. The then-branch is synth (emitted); the else-branch is checked
/// against the result and *not* emitted (a coercion-possible position). The
/// condition is left to name resolution (not typed).
#[test]
fn if_then_else_result_matches_fcs() {
    // Emitted: the then `1` and the whole `if … else 2` expression — both int.
    // The condition `true` and the else `2` are not emitted.
    let source = "module I\nlet r = if true then 1 else 2\n";
    let inferred = infer_src(source);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() == "System.Int32"),
        "the if-expression and its then-branch are both int: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        assert_sound(source),
        2,
        "then-branch `1` + the if-expression; the else `2` is check-mode (deferred)"
    );
}

/// The `if` result type flows to the binder and on to its uses.
#[test]
fn if_result_flows_to_uses() {
    let source = "module I\nlet r = if true then 1 else 2\nlet s = r\n";
    // then `1` + the if-expression + the use `r` in `let s = r` — three int nodes.
    assert_eq!(assert_sound(source), 3);
}

/// Soundness regression (3.2c-1): when the then-branch can't be synthesized the
/// whole `if` must defer — its type must *not* be taken from the coercible
/// else-branch. `box 1 : obj` is an application we don't type, so even though the
/// else `"s"` is a bare string literal, the if's type is unknown (FCS: `obj`) and
/// nothing may be emitted. (Before the fix the deferred then-branch let `"s"`
/// drive the if to `string`.)
#[test]
fn if_then_uninferrable_defers_whole_expression() {
    let source = "module I\nlet v = if true then box 1 else \"s\"\n";
    let inferred = infer_src(source);
    assert!(
        inferred.is_empty(),
        "then-branch `box 1` is untypeable, so the `if` defers; got {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_eq!(assert_sound(source), 0);
}

/// Soundness regression (3.2c-1): a check-mode tuple's *elements* are themselves
/// in check positions, not synth. The else-branch `(3, 4)` is checked against the
/// then-branch's `int64 * int64`, so FCS coerces `3`/`4` to `int64`; emitting them
/// as `int` (their synthesized type) would be wrong. We suppress them — there are
/// no `int` literals anywhere, so any `System.Int32` emission would be the bug.
#[test]
fn check_mode_tuple_elements_are_not_emitted() {
    let source = "module I\nlet b = true\nlet v = if b then (1L, 2L) else (3, 4)\n";
    let inferred = infer_src(source);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() != "System.Int32"),
        "else-tuple elements `3`/`4` are check-mode (coerced to int64), not int: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    // The then-tuple `(1L, 2L)` and the `if` are still synthesized as int64 * int64.
    assert!(
        inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Int64 * System.Int64"),
        "the then-tuple and the if-expression are int64 * int64: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    // assert_sound is the real guard: every emitted type agrees with FCS.
    assert_sound(source);
}

/// Soundness regression (3.2c-1): a nested `if` in an else-branch is in check
/// mode, so *its* then-branch is too. In `if c then 1L else if d then 2 else 3L`
/// the inner then `2` is checked (FCS coerces it to `int64`); emitting it as `int`
/// would be wrong. No `int` literal exists, so any `System.Int32` is the bug.
#[test]
fn nested_check_mode_if_then_is_not_emitted() {
    let source =
        "module I\nlet c = true\nlet d = false\nlet v = if c then 1L else if d then 2 else 3L\n";
    let inferred = infer_src(source);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() != "System.Int32"),
        "nested check-mode then `2` is coerced to int64, not int: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_sound(source);
}

/// An else-less `if c then a` desugars to `if c then a else ()`: its result is
/// `unit` and the then-branch sits in a unit *check* position, so the synthesized
/// then-branch type is not the if's type. We defer the whole `if` (no emission)
/// rather than risk emitting the then-branch's type for the result.
#[test]
fn else_less_if_is_deferred() {
    let source = "module I\nlet c = true\nlet v = if c then ()\n";
    let inferred = infer_src(source);
    // Only `c = true` (bool) is typed; nothing is emitted for the else-less `if`.
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() == "System.Boolean"),
        "else-less if is deferred; only the bool binder is typed: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_sound(source);
}

/// Soundness regression (3.2c-1): an `elif` chain with **no final `else`** is the
/// same no-else shape as `if c then a`. `if a then 1 elif b then 2` parses as
/// `if a then 1 else (if b then 2)` — the outer `if` *has* an immediate
/// else-branch (the nested else-less `if`), so a non-recursive check would wrongly
/// synth it to `int`. It has no *final* else, its result is `unit`, and FCS
/// rejects it, so we must defer: no `System.Int32` may be emitted.
#[test]
fn elif_without_final_else_is_deferred() {
    let source = "module I\nlet a = true\nlet b = true\nlet v = if a then 1 elif b then 2\n";
    let inferred = infer_src(source);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() == "System.Boolean"),
        "elif chain without a final else is deferred; only the bool binders type: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_sound(source);
}

/// The companion to `elif_without_final_else_is_deferred`: an `elif` chain that
/// *does* end in a genuine `else` is synthesized to the then-branch type and
/// agrees with FCS. The inner `elif`/`else` bodies are check-mode (not emitted).
#[test]
fn elif_with_final_else_matches_fcs() {
    let source = "module I\nlet a = true\nlet b = true\nlet v = if a then 1 elif b then 2 else 3\n";
    let inferred = infer_src(source);
    // The then `1` and the whole `if` are int; `2`/`3` are check-mode (deferred).
    assert!(
        inferred
            .types()
            .values()
            .filter(|t| t.render() == "System.Int32")
            .count()
            == 2,
        "the then `1` and the if-expression are int; `2`/`3` are not emitted: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_sound(source);
}

/// 3.2c-2a: a **function-binding body** is now walked, so a body that is an `if`
/// emits its result type. `let f c = if c then 1 else 2` types the then-branch `1`
/// and the whole `if` as `int` (the function's return). The condition `c` is not
/// typed here (see the module docs); the function value `f` stays Deferred.
#[test]
fn function_body_result_is_typed() {
    let source = "module I\nlet f c = if c then 1 else 2\n";
    let inferred = infer_src(source);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() == "System.Int32"),
        "the body `if`/then are int; the condition and `f` are not typed: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    // then `1` + the `if` (both int); the condition `c` and else `2` are not emitted.
    assert_eq!(assert_sound(source), 2);
}

/// The function-head test spans both `SynArgPats` shapes, matching `binders`: a
/// head whose only argument is a **named-field group** (`let f (a = x) = …`, where
/// `args()` is empty and `name_pat_pairs()` is `Some`) is still a function binding,
/// so its body is walked. Asserted on our own output — the named-field pattern
/// needs a record type to be well-typed, so the FCS oracle isn't meaningful here;
/// the point is that the body's literal is reached at all (it would be Deferred if
/// the guard only checked `args()`).
#[test]
fn named_field_function_head_body_is_walked() {
    let source = "module I\nlet f (a = x) = 42\n";
    let inferred = infer_src(source);
    assert!(
        inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Int32"),
        "the named-field-head function body `42` should be walked and typed: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
}

/// The body traversal reaches through a **lambda** too: `let g = fun c -> if c
/// then 1 else 2` walks the lambda body, emitting the `if` result (`int`).
#[test]
fn lambda_body_result_is_typed() {
    let source = "module I\nlet g = fun c -> if c then 1 else 2\n";
    let inferred = infer_src(source);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() == "System.Int32"),
        "the lambda body `if`/then are int: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_eq!(assert_sound(source), 2);
}

/// Soundness (3.2c-2a): a lambda reached in **check mode** carries that mode into
/// its body — a bare literal body must not be synth-emitted, since an expected
/// function type can retarget it. Here `fun _ -> 2` is the else-branch checked
/// against `f` (`_ -> int64`), so FCS coerces `2` to `int64`; emitting it as `int`
/// (synth) would be wrong. The synth-position lambda body `1L` in
/// `let f = fun _ -> 1L` still emits (int64). No `int` may appear.
#[test]
fn lambda_body_in_check_mode_is_not_synth_emitted() {
    let source = "module I\nlet f = fun _ -> 1L\nlet v = if true then f else (fun _ -> 2)\n";
    let inferred = infer_src(source);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() != "System.Int32"),
        "the check-mode lambda body `2` (coerced to int64) must not emit as int: {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    // Only `1L` (int64, f's synth-mode body); the else lambda's `2` is check-mode
    // (deferred), and the functions `f`/`v` stay Deferred.
    assert_eq!(assert_sound(source), 1);
}

/// Soundness (3.2c-2a): a `while` body is checked against `unit`, a check (not
/// synth) position. `while c do if c then 1 else 2` has a non-unit body (FCS
/// coerces the region to `unit`), so the inner `if`/`1`/`2` must not be
/// synth-emitted — nothing is emitted for the body.
#[test]
fn while_body_is_unit_check_position() {
    let source = "module I\nlet w c = while c do if c then 1 else 2\n";
    let inferred = infer_src(source);
    assert!(
        inferred.is_empty(),
        "the unit-check `while` body is not synth-emitted; got {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_eq!(assert_sound(source), 0);
}

/// A **ground application** emits the result type at the *application* node (Stage
/// 3.2c-3): `let f c = if c then 1 else 2` (`f : bool -> int`) then `let n = f
/// true` — the node at `f true` is `int`, matching FCS's `call:function` node.
/// The bare function-position `f` is **not** emitted (FCS has no node there — the
/// probe finding), nor is the checked argument `true`; only the application node
/// and the function body's `int`s are typed.
#[test]
fn ground_application_node_is_typed() {
    let source = "module I\nlet f c = if c then 1 else 2\nlet n = f true\n";
    let inferred = infer_src(source);
    // The application `f true` is `int`; no node is a function type or bool here
    // (the function-position `f` is not emitted, the argument `true` is checked).
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() == "System.Int32"),
        "every emitted type is int (body + application); got {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    // body then `1` + the body `if` + the application `f true` = 3 int nodes.
    assert_eq!(assert_sound(source), 3);
}

/// A **partial application** emits its residual function type at the application
/// node: `let add a b = …` then `let g = add true` — the node at `add true` is
/// `bool -> int`, matching FCS's node. The application does not clear
/// walk-completeness; `g`'s value binding just picks up the ground residual.
#[test]
fn partial_application_node_is_typed() {
    let source =
        "module I\nlet add a b = if a then (if b then 1 else 2) else 3\nlet g = add true\n";
    let inferred = infer_src(source);
    // The residual `add true : bool -> int` is emitted at the application node.
    assert!(
        inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Boolean -> System.Int32"),
        "the partial application node should be `bool -> int`; got {:?}",
        inferred
            .types()
            .values()
            .map(Ty::render)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        assert_sound(source),
        inferred.len(),
        "everything we emit agrees with FCS"
    );
}

/// A `match` whose arm bodies disagree, or whose result is in a coercion
/// position, is not yet modelled — `match` typing is a later slice — so nothing
/// is emitted for the `match` itself. Pins that we don't accidentally type it.
#[test]
fn match_expression_is_deferred() {
    let source = "module I\nlet r = match 0 with _ -> 1\n";
    // We don't model `match` yet; `assert_sound` confirms whatever we *do* emit
    // (here: nothing for the match result) agrees with FCS.
    let inferred = infer_src(source);
    assert!(
        inferred.is_empty(),
        "match is not modelled yet; expected no inferred types, got {}",
        inferred.len()
    );
    assert_eq!(assert_sound(source), 0);
}

/// Literals in positions where an expected type can retarget them must be
/// deferred. Each snippet's only literal sits in an unsound position, so nothing
/// is inferred — and `assert_sound` confirms we never emit a wrong type there.
#[test]
fn unsound_positions_are_deferred() {
    let snippets = [
        // Return-type annotation coerces the unsuffixed literal to int64: the
        // literal *node* stays deferred (the binder is typed from the
        // annotation — Stage R2-a — but that is the `def_types` view, not an
        // expression node, and this test asserts the expression map).
        "module M\nlet x : int64 = 42\n",
        // Typed pattern coerces likewise.
        "module M\nlet (x : int64) = 42\n",
        // Format context retypes the string to a PrintfFormat.
        "module M\nlet p = printfn \"%d\" 1\n",
        // Unit-of-measure literal: the value is a *measured* type Ty cannot model.
        "module M\nlet x = 1.0<kg>\n",
        // Collection element: unifies with the other elements' type.
        "module M\nlet xs = [ 1; 2L ]\n",
        // Recursive group: bindings are solved together, so a sibling's
        // constraints can flow back to a binder's literal RHS — not isolated.
        "module M\nlet rec x = 1\nand y = x\n",
        "module M\nlet rec loop n = if n then 1L else x\nand x = 2\n",
    ];
    for source in snippets {
        let inferred = infer_src(source);
        assert!(
            inferred.is_empty(),
            "expected no inferred types (all literals in unsound positions): {source:?}, \
             got {} type(s)",
            inferred.len()
        );
        // Redundant given emptiness, but pins that we are not merely *missing* a
        // node we'd have got wrong: if a future change starts typing one of
        // these, this catches a *wrong* type, not just a count change.
        assert_eq!(assert_sound(source), 0);
    }
}

/// Argument position: the literal's type is the parameter's *expected* type
/// (`let f (x: int64) = x` then `f 42` retargets the default `int` to
/// `int64`), so the literal node must stay silent. Since R2-b the surrounding
/// binding does emit other nodes — `f`'s body use `x` (`int64`) and the
/// ground application result — so the pin is range-precise on the literal,
/// and `assert_sound` confirms everything we do emit agrees with FCS.
#[test]
fn argument_position_literal_is_deferred() {
    let source = "module M\nlet f (x: int64) = x\nlet y = f 42\n";
    let inferred = infer_src(source);
    let lit_at = source.rfind("42").expect("literal");
    assert!(
        inferred
            .types()
            .keys()
            .all(|r| u32::from(r.start()) as usize != lit_at),
        "the argument-position literal must stay deferred"
    );
    assert_sound(source);
}

/// User-defined numeric literals (`1I`/`1G`, suffix-/module-dependent) and
/// source-location identifiers have no kind-fixed type even in a sound position,
/// so they are deferred there too.
#[test]
fn user_numeric_literals_are_deferred() {
    let inferred = infer_src("module M\nlet c = 1I\n");
    assert!(
        inferred.is_empty(),
        "user-defined numeric literal `1I` must be deferred"
    );
}

proptest! {
    /// In a sound position, an unsuffixed integer literal is `System.Int32` and a
    /// fractional one is `System.Double` — the defaults, for any value.
    #[test]
    fn bare_rhs_unsuffixed_numerics_typed(n in 0u32..100_000) {
        for (src, want) in [
            (format!("module M\nlet x = {n}\n"), "System.Int32"),
            (format!("module M\nlet x = {n}.5\n"), "System.Double"),
        ] {
            let parsed = parse(&src);
            prop_assume!(parsed.errors.is_empty());
            let file = ImplFile::cast(parsed.root).expect("impl file");
            let inf = infer(&file);
            prop_assert_eq!(inf.len(), 1, "exactly one literal in {:?}", src);
            let got = inf.types().values().next().expect("one type").render();
            prop_assert_eq!(got, want.to_string(), "src={:?}", src);
        }
    }

    /// In a sound position, a suffixed integer literal types to exactly the
    /// primitive its suffix names — the kind→`Ty` table is correct and total.
    #[test]
    fn suffixed_integers_type_by_suffix(n in 0u32..100) {
        let cases = [
            ("y", "System.SByte"),
            ("uy", "System.Byte"),
            ("s", "System.Int16"),
            ("us", "System.UInt16"),
            ("u", "System.UInt32"),
            ("L", "System.Int64"),
            ("UL", "System.UInt64"),
            ("n", "System.IntPtr"),
            ("un", "System.UIntPtr"),
        ];
        for (sfx, ty) in cases {
            let src = format!("module M\nlet x = {n}{sfx}\n");
            let parsed = parse(&src);
            prop_assume!(parsed.errors.is_empty());
            let file = ImplFile::cast(parsed.root).expect("impl file");
            let inf = infer(&file);
            prop_assert_eq!(inf.len(), 1, "exactly one literal in {:?}", src);
            let got = inf.types().values().next().expect("one type").render();
            prop_assert_eq!(got, ty.to_string(), "src={:?}", src);
        }
    }

    /// A return-type annotation can retarget the literal, so the literal's
    /// *expression node* is always deferred — but the annotation types the
    /// **binder** (Stage R2-a): `let x : int64 = <n>` has `x : System.Int64`
    /// for any value, while the RHS literal node stays absent. (Flipped from
    /// the pre-R2-a `annotated_bindings_always_deferred`, which pinned that the
    /// whole binding deferred.)
    #[test]
    fn annotated_bindings_type_the_binder_not_the_literal(n in 0u32..100_000) {
        let src = format!("module M\nlet x : int64 = {n}\n");
        let parsed = parse(&src);
        prop_assume!(parsed.errors.is_empty());
        let file = ImplFile::cast(parsed.root).expect("impl file");
        // The full BCL env: `int64` types through FSharp.Core's abbreviation
        // marker and the target chase, not a hard-coded alias table.
        let env = crate::common::full_bcl_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), env);
        let inferred = infer_file(&file, &resolved, env);
        prop_assert!(
            inferred.types().is_empty(),
            "the literal node must stay deferred: {:?}",
            src
        );
        let x_ty = inferred
            .def_types()
            .iter()
            .find(|(id, _)| resolved.def(**id).name == "x")
            .map(|(_, ty)| ty.render());
        prop_assert_eq!(x_ty, Some("System.Int64".to_string()), "src={:?}", src);
    }

    /// A recursive binding group is never isolated, so its literal RHS is always
    /// deferred regardless of value.
    #[test]
    fn recursive_bindings_always_deferred(n in 0u32..100_000) {
        let src = format!("module M\nlet rec x = {n}\nand y = x\n");
        let parsed = parse(&src);
        prop_assume!(parsed.errors.is_empty());
        let file = ImplFile::cast(parsed.root).expect("impl file");
        prop_assert!(infer(&file).is_empty(), "should defer rec {:?}", src);
    }

    /// A use of an `int`-bound value is itself `System.Int32`, for any value —
    /// the binder's type propagates to the use. Both the literal RHS and the use
    /// `x` are typed (two nodes); the binder `y` is a definition, not an
    /// expression, so it is not itself in the map.
    #[test]
    fn value_use_inherits_int_binding(n in 0u32..100_000) {
        let src = format!("module M\nlet x = {n}\nlet y = x\n");
        let parsed = parse(&src);
        prop_assume!(parsed.errors.is_empty());
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let inf = infer(&file);
        prop_assert_eq!(inf.len(), 2, "literal RHS + the use of `x`: {:?}", src);
        for ty in inf.types().values() {
            prop_assert_eq!(ty.render(), "System.Int32".to_string(), "src={:?}", src);
        }
    }
}
