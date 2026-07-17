//! Differential test (`parser::parse` vs FCS): list `[ … ]` and array
//! `[| … |]` literal *expressions* — FCS's `listExpr` (`pars.fsy:5298`) /
//! `arrayExpr` (`:5450`). An empty bracket is `SynExpr.ArrayOrList(isArray,
//! [], _)`; a non-empty one is `SynExpr.ArrayOrListComputed(isArray, body, _)`
//! whose `body` is the single `sequentialExpr` (a `Sequential` for two-or-more
//! `;`/offside-separated elements). The element separator is `;`, not `,`.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

// ---- empty brackets → SynExpr.ArrayOrList(_, [], _) ---------------------

/// Empty list `[]` → `ArrayOrList(isArray = false, [], _)`.
#[test]
fn diff_ast_list_empty() {
    assert_asts_match("let x = []\n");
}

/// Empty array `[||]` → `ArrayOrList(isArray = true, [], _)`.
#[test]
fn diff_ast_array_empty() {
    assert_asts_match("let x = [||]\n");
}

/// Empty list with interior whitespace `[ ]` — still empty (trivia only).
#[test]
fn diff_ast_list_empty_spaced() {
    assert_asts_match("let x = [ ]\n");
}

/// Empty array with interior whitespace `[| |]` — still empty.
#[test]
fn diff_ast_array_empty_spaced() {
    assert_asts_match("let x = [| |]\n");
}

// ---- single element → ArrayOrListComputed(_, <elem>, _) -----------------

/// Single-element list `[1]` → `ArrayOrListComputed(false, Const 1, _)` — a
/// bare element body, no `Sequential` wrapper.
#[test]
fn diff_ast_list_single() {
    assert_asts_match("let x = [1]\n");
}

/// Single-element array `[| 1 |]` → `ArrayOrListComputed(true, Const 1, _)`.
#[test]
fn diff_ast_array_single() {
    assert_asts_match("let x = [| 1 |]\n");
}

/// Single ident element `[x]` — the body is `SynExpr.Ident`.
#[test]
fn diff_ast_list_single_ident() {
    assert_asts_match("let x = [y]\n");
}

// ---- `;`-separated elements → Sequential body ---------------------------

/// `[1; 2; 3]` → `ArrayOrListComputed(false, Sequential[1; 2; 3], _)`.
#[test]
fn diff_ast_list_semi_separated() {
    assert_asts_match("let x = [1; 2; 3]\n");
}

/// `[| 1; 2 |]` → `ArrayOrListComputed(true, Sequential[1; 2], _)`.
#[test]
fn diff_ast_array_semi_separated() {
    assert_asts_match("let x = [| 1; 2 |]\n");
}

/// A trailing `;` before the closer is tolerated (FCS parses it cleanly, with
/// no extra element).
#[test]
fn diff_ast_list_trailing_semi() {
    assert_asts_match("let x = [1; 2;]\n");
}

// ---- element separator is `;`, not `,` ----------------------------------

/// `[1, 2]` is a **one**-element list whose element is the tuple `(1, 2)` —
/// `ArrayOrListComputed(false, Tuple[1; 2], _)`. Pins that `,` binds inside the
/// element (the `parse_expr` tuple layer) rather than separating elements.
#[test]
fn diff_ast_list_comma_is_tuple_element() {
    assert_asts_match("let x = [1, 2]\n");
}

/// `[1, 2; 3, 4]` — two tuple elements separated by `;`:
/// `Sequential[Tuple[1; 2]; Tuple[3; 4]]`.
#[test]
fn diff_ast_list_tuple_elements() {
    assert_asts_match("let x = [1, 2; 3, 4]\n");
}

// ---- elements are full expressions --------------------------------------

/// Infix `+` binds tighter than the `;` separator: `[1 + 1; f y]`.
#[test]
fn diff_ast_list_expr_elements() {
    assert_asts_match("let x = [1 + 1; f y]\n");
}

/// Bool / string element literals.
#[test]
fn diff_ast_list_literal_elements() {
    assert_asts_match("let x = [true; false]\n");
}

// ---- composition with the surrounding expression grammar ----------------

/// A list is atomic-precedence, so it stands in application-argument position:
/// `f [1; 2]` → `App(f, ArrayOrListComputed(…))`.
#[test]
fn diff_ast_list_as_app_arg() {
    assert_asts_match("let x = f [1; 2]\n");
}

/// The reported failing case: a bracket argument after an adjacent paren-app
/// argument — `dumpAst (g x) []` → `App(App(dumpAst, (g x)), [])`.
#[test]
fn diff_ast_list_empty_as_trailing_app_arg() {
    assert_asts_match("let x = dumpAst (g x) []\n");
}

/// A list literal is a valid infix operand: `[1] @ [2]` (list concatenation).
#[test]
fn diff_ast_list_infix_operand() {
    assert_asts_match("let x = [1] @ [2]\n");
}

/// Postfix dot binds the whole list: `[1].Length` → `DotGet([1], ["Length"])`.
#[test]
fn diff_ast_list_postfix_dot() {
    assert_asts_match("let x = [1].Length\n");
}

/// A list as a tuple element: `([1], [2])` → `Paren(Tuple[[1]; [2]])`.
#[test]
fn diff_ast_list_as_tuple_element() {
    assert_asts_match("let x = ([1], [2])\n");
}

/// A list inside parentheses `([1; 2])` → `Paren(ArrayOrListComputed(…))`.
#[test]
fn diff_ast_list_in_parens() {
    assert_asts_match("let x = ([1; 2])\n");
}

/// A list in each branch of an `if`/`else`.
#[test]
fn diff_ast_list_if_branches() {
    assert_asts_match("let x = if c then [1] else [2]\n");
}

// ---- nesting ------------------------------------------------------------

/// Nested lists `[ [1]; [2] ]` — each element is itself a list.
#[test]
fn diff_ast_list_nested() {
    assert_asts_match("let x = [ [1]; [2] ]\n");
}

/// Nested with no interior spacing `[[1]; [2]]`.
#[test]
fn diff_ast_list_nested_tight() {
    assert_asts_match("let x = [[1]; [2]]\n");
}

/// A list of arrays `[ [|1|]; [|2|] ]` — mixed delimiters nest.
#[test]
fn diff_ast_list_of_arrays() {
    assert_asts_match("let x = [ [|1|]; [|2|] ]\n");
}

// ---- offside (newline-separated) elements -------------------------------

/// Newline-separated elements inside an offside `let` RHS block — the elements
/// are separated by `OBLOCKSEP`, still producing one `Sequential` body.
#[test]
fn diff_ast_list_offside_elements() {
    assert_asts_match("let x =\n    [ 1\n      2\n      3 ]\n");
}

// ---- computation/yield bodies (the existing expr surface composes) ------
//
// FCS lowers these to `ArrayOrListComputed` as well; because the body is a
// plain `sequentialExpr` and our `parse_expr` already handles `yield` / `for`,
// they fall out for free and diff-match FCS.

/// `[ yield 1 ]` — the body is a `YieldOrReturn`.
#[test]
fn diff_ast_list_yield() {
    assert_asts_match("let x = [ yield 1 ]\n");
}

/// `[ yield 1; yield 2 ]` — a `Sequential` of two `YieldOrReturn`s.
#[test]
fn diff_ast_list_yield_seq() {
    assert_asts_match("let x = [ yield 1; yield 2 ]\n");
}

/// `[ for y in ys -> y ]` — a `for … ->` comprehension body (`ForEach` with an
/// implicit-`yield` `-> y`).
#[test]
fn diff_ast_list_for_arrow() {
    assert_asts_match("let x = [ for y in ys -> y ]\n");
}

/// `[ for y in ys do yield y ]` — the `for … do yield …` comprehension body.
#[test]
fn diff_ast_list_for_do_yield() {
    assert_asts_match("let x = [ for y in ys do yield y ]\n");
}

// ---- range comprehensions (phase 10.20) --------------------------------

/// A form FCS rejects: must round-trip losslessly and surface a parse error
/// (so `assert_asts_match` is N/A — used by the attribute-position cases that
/// stay clean errors).
fn assert_clean_error(source: &str) {
    let parsed = parse(source);
    assert_eq!(
        parsed.root.text().to_string(),
        source,
        "lossless round-trip violated for {source:?}",
    );
    assert!(
        !parsed.errors.is_empty(),
        "expected a parse error for {source:?}, got none",
    );
}

/// `[1..10]` (range comprehension) — once 10.20 added the `..` range
/// expression, the body's `parse_expr` reaches `parse_range_expr`, so this is
/// `ArrayOrListComputed(IndexRange(Some 1, Some 10))`. (Was a 10.19 clean-error
/// deferral.) Broader range coverage lives in `parser_diff_ranges.rs`.
#[test]
fn diff_ast_list_range() {
    assert_asts_match("let x = [1..10]\n");
}

// ---- after-type (attribute / inherit) argument position -----------------
//
// FCS's `atomicExprAfterType` (the attribute / `inherit`-constructor argument)
// includes `arrayExpr` (`[| … |]`) but **not** `listExpr` (`[ … ]`), so an
// unparenthesised array arg is valid while a bare list arg is FS0010
// "Unexpected symbol '[' in attribute list".

/// `[<Foo [|1|]>]` — an unparenthesised *array* attribute argument is valid.
#[test]
fn diff_ast_attr_array_arg() {
    assert_asts_match("[<Foo [|1|]>]\nlet x = 1\n");
}

/// `inherit B [|1|]` — an unparenthesised *array* constructor argument is valid.
#[test]
fn diff_ast_inherit_array_arg() {
    assert_asts_match("type C() =\n    inherit B [|1|]\n");
}

/// `[<Foo([1])>]` — a *parenthesised* list attribute argument is valid (head
/// token `(`), unaffected by the bare-list exclusion.
#[test]
fn diff_ast_attr_paren_list_arg() {
    assert_asts_match("[<Foo([1])>]\nlet x = 1\n");
}

/// `[<Foo [1]>]` — a bare *list* attribute argument is rejected by FCS (FS0010)
/// and by us. Both error, but FCS's error-recovery AST need not match ours, so
/// this only pins the clean (non-panicking) rejection.
#[test]
fn diff_ast_attr_bare_list_rejected() {
    assert_clean_error("[<Foo [1]>]\nlet x = 1\n");
}

/// A typed `yield`/`yield!` element in a list/array — FCS's
/// `YIELD declExpr COLON typ` (`pars.fsy:4488`) binds the `: T` *inside* the
/// yield (`Yield(Typed(e, T))`). A *plain* bracket element is `sequentialExpr`
/// (no bare annotation — `[1 : int]` is rejected), so this annotation must be
/// carried by the yield production, not the element gatherer.
#[test]
fn diff_ast_list_typed_yield_element() {
    assert_asts_match("let xs = [yield 1 : int]\n");
    assert_asts_match("let xs = [|yield 1 : int|]\n");
    assert_asts_match("let xs = [yield! ys : int]\n");
    assert_asts_match("let xs = seq { yield 1 : int }\n");
}
