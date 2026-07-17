//! Differential test (`parser::parse` vs FCS): *object expressions*
//! `{ new T(args) with member … }` — FCS's `SynExpr.ObjExpr(objType,
//! argOptions, withKeyword, bindings, members, extraImpls, newExprRange, range)`
//! (`pars.fsy:5828`, `SyntaxTree.fsi:645`).
//!
//! Distinct from the object-*construction* `new T(args)` (`SynExpr.New`,
//! `parser_diff_new_expr.rs`): an object expression is a brace-delimited
//! anonymous implementation with `with member …` overrides. **Stage A** covers
//! the `with member …` member form (the reported bug); the `with`-`localBindings`
//! form (`{ new T() with X = e }`), extra `interface I with …` implementations,
//! and the bare no-parens `{ new T }` form are later stages.
//!
//! The key disambiguation pinned here: a `new`-headed brace is an object
//! expression only when a `with` block follows the base call — `{ new T(1, 2) }`
//! (no `with`) stays a `ComputationExpr(New …)`, which the regression tests at
//! the bottom guard.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

// ---- the reported bug: the bare `new T with member …` form ---------------

/// The motivating case from the report: an interface object expression with no
/// constructor parens (`argOptions = None`) and a single unit-returning member.
#[test]
fn diff_ast_obj_expr_interface_member() {
    assert_asts_match("let x =\n    { new IDisposable with\n        member x.Dispose () = () }\n");
}

/// A value member (no method parens) — `member x.P = 1`.
#[test]
fn diff_ast_obj_expr_value_member() {
    assert_asts_match("let x = { new IFoo with member x.P = 1 }\n");
}

/// A wildcard `this` (`member _.M`).
#[test]
fn diff_ast_obj_expr_wildcard_this() {
    assert_asts_match("let x = { new IFoo with member _.M () = 0 }\n");
}

/// Two members in the block.
#[test]
fn diff_ast_obj_expr_two_members() {
    assert_asts_match(
        "let x =\n    { new IFoo with\n        member x.M = 1\n        member x.N = 2 }\n",
    );
}

/// An object-expression member block closed by an explicit `end` before the `}`
/// (`{ new IFoo with member … end }`). FCS accepts this (`ParseHadErrors: false`);
/// the `end` is an inert `END_TOK` and the projection matches the brace-only form.
/// The shared augment helper claims the `end` (a non-empty block always does), so
/// the object expression's `}` closer still lands on the brace. (An *empty*
/// `{ new IFoo with end }` is an FCS parse error — the helper is passed
/// `empty_block_takes_end = false`, leaving that `end` to stray-token recovery.)
#[test]
fn diff_ast_obj_expr_member_end() {
    assert_asts_match("let x = { new IFoo with member _.M = 1 end }\n");
}

// ---- with a constructor call (`argOptions = Some`) ------------------------

/// `new T() with …` — a unit constructor argument (`argOptions = Some(Const
/// Unit, None)`), the form that distinguishes an object expression with a base
/// call from the bare interface form.
#[test]
fn diff_ast_obj_expr_unit_ctor() {
    assert_asts_match("let x = { new System.Object() with member x.ToString () = \"hi\" }\n");
}

/// A constructor with real arguments (`argOptions = Some(Paren(Tuple …), None)`).
#[test]
fn diff_ast_obj_expr_ctor_args() {
    assert_asts_match("let x = { new Foo(1, 2) with member x.M () = () }\n");
}

// ---- type-surface variety on `objType` -----------------------------------

/// A dotted object type — `new System.IDisposable with …`.
#[test]
fn diff_ast_obj_expr_dotted_type() {
    assert_asts_match("let x = { new System.IDisposable with member x.Dispose () = () }\n");
}

/// A generic object type — the `<…>` stays inside the `atomType`.
#[test]
fn diff_ast_obj_expr_generic_type() {
    assert_asts_match("let x = { new IComparable<int> with member x.CompareTo o = 0 }\n");
}

// ---- member-body variety -------------------------------------------------

/// A member body that is a non-trivial expression.
#[test]
fn diff_ast_obj_expr_expr_body() {
    assert_asts_match("let x = { new IFoo with member x.M () = 1 + 2 }\n");
}

/// A member with a curried parameter referenced in the body.
#[test]
fn diff_ast_obj_expr_member_param() {
    assert_asts_match("let x = { new IFoo with member _.Add a b = a + b }\n");
}

// ---- the base alias `as base` (`baseSpec`) -------------------------------

/// The *valid* base-alias form — a quoted `` ``base`` `` identifier
/// (`argOptions = Some(_, Some base)`). FCS accepts it without error; the alias
/// is elided by the normaliser, so the shape matches the no-alias form.
#[test]
fn diff_ast_obj_expr_base_alias_quoted() {
    assert_asts_match("let x = { new System.Object() as ``base`` with member x.M () = 1 }\n");
}

// ---- extra interface implementations (`extraImpls`) ----------------------

/// A `with member …` block followed by one `interface I with member …`
/// implementation — FCS's `objExpr` alt 1 with `opt_objExprInterfaces`. The
/// member lands in `members`, the interface in `extraImpls`.
#[test]
fn diff_ast_obj_expr_one_extra_interface() {
    assert_asts_match(
        "let x =\n    { new System.Object() with\n        member x.ToString () = \"hi\"\n      interface IDisposable with\n        member x.Dispose () = () }\n",
    );
}

/// Two extra interface implementations after the member block.
#[test]
fn diff_ast_obj_expr_two_extra_interfaces() {
    assert_asts_match(
        "let x =\n    { new System.Object() with\n        member x.M () = ()\n      interface IA with\n        member x.A () = ()\n      interface IB with\n        member x.B () = () }\n",
    );
}

/// An extra interface with two members of its own.
#[test]
fn diff_ast_obj_expr_extra_interface_two_members() {
    assert_asts_match(
        "let x =\n    { new System.Object() with\n        member x.M () = ()\n      interface IFoo with\n        member x.A () = ()\n        member x.B () = () }\n",
    );
}

/// A dotted/generic interface type in the extra-impl position.
#[test]
fn diff_ast_obj_expr_extra_interface_generic_type() {
    assert_asts_match(
        "let x =\n    { new System.Object() with\n        member x.M () = ()\n      interface System.IComparable<int> with\n        member x.CompareTo o = 0 }\n",
    );
}

/// An object expression with an extra interface, followed by a sibling
/// declaration — the interface loop must close cleanly and leave the enclosing
/// block's separator for the module loop (the Stage A sibling discipline,
/// extended to the interface tail).
#[test]
fn diff_ast_obj_expr_extra_interface_then_sibling() {
    assert_asts_match(
        "let x =\n    { new System.Object() with\n        member x.M () = ()\n      interface IDisposable with\n        member x.Dispose () = () }\nlet y = 1\n",
    );
}

// ---- the interface-only form (FCS `objExpr` alt 2) -----------------------

/// `{ new T() interface I with member … }` — no `with member` block, the
/// interface directly follows the base call (FCS's `objExprBaseCall
/// objExprInterfaces`). FCS accepts this single-line layout: an `ObjExpr` with
/// no `members` and one `extraImpls`.
#[test]
fn diff_ast_obj_expr_interface_only() {
    assert_asts_match(
        "let x = { new System.Object() interface System.IDisposable with member _.Dispose () = () }\n",
    );
}

/// Interface-only with the interface indented *deeper* than the brace (no
/// `OBLOCKSEP` between the base call and the interface) — FCS accepts this.
#[test]
fn diff_ast_obj_expr_interface_only_deep_indent() {
    assert_asts_match(
        "let x =\n    { new System.Object()\n        interface System.IDisposable with\n          member _.Dispose () = () }\n",
    );
}

/// Interface-only with a *second* single-line interface — FCS rejects this
/// (FS0010 on the second `interface`: a single-line interface-only form admits
/// only one interface). Our parser is lenient here (the offside check FCS uses
/// is not replicated), so this is a parser-level lossless-recovery assertion
/// rather than a clean diff — same class as the documented repeated-separator /
/// bare-`*` leniencies. The well-formed tree never corrupts; the only gap is the
/// missing diagnostic.
#[test]
fn obj_expr_interface_only_two_single_line_is_lenient() {
    let src = "let x = { new System.Object() interface IA with member _.A () = () interface IB with member _.B () = () }\n";
    let parse = parse(src);
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless on the lenient two-interface single-line form",
    );
}

// ---- the mis-nesting guard: an object expression inside a type member body --

/// An object expression in a type member body, where the *type* then has its
/// own `interface …` implementation. The object expression's `}` is swallowed,
/// so its (raw-guarded) interface loop must not steal the type-level interface:
/// the object expression has *no* `extraImpls`, and the type carries the
/// interface as a member (9.11b).
#[test]
fn diff_ast_obj_expr_in_member_body_then_type_interface() {
    assert_asts_match(
        "type T() =\n    member _.M = { new IFoo with member _.F () = () }\n    interface IDisposable with\n        member _.Dispose () = ()\n",
    );
}

// ---- the object expression in expression positions -----------------------

/// As a function argument — the whole `{ … }` is one atom.
#[test]
fn diff_ast_obj_expr_as_arg() {
    assert_asts_match("let x = ignore { new IFoo with member _.M () = () }\n");
}

/// Followed by a sibling declaration — the object expression must close cleanly
/// and leave the enclosing block's separator for the module loop (no absorbing
/// the following `let`).
#[test]
fn diff_ast_obj_expr_then_sibling() {
    assert_asts_match(
        "let x =\n    { new IDisposable with\n        member x.Dispose () = () }\nlet y = 1\n",
    );
}

// ---- nested: an inner `new`-headed brace must not steal the outer `with` --

/// A `new`-headed *computation* brace nested inside an object expression's
/// constructor argument: `{ new Foo(seq { new Bar() }) with member _.M = () }`.
/// The inner `seq { new Bar() }`'s `}` and the enclosing `)` are LexFilter-
/// swallowed, so the inner brace's filtered lookahead sees the *outer* `with`.
/// The inner must still be a `ComputationExpr(New Bar)` and the outer the
/// `ObjExpr`, not the inner stealing the outer member block.
#[test]
fn diff_ast_obj_expr_nested_new_computation_arg() {
    assert_asts_match("let x = { new Foo(seq { new Bar() }) with member _.M = () }\n");
}

// ---- the value-binding form `{ new T() with X = e }` (FCS `objExprBindings`) -

/// The motivating `localBindings` form — a single value binding `with X = e`
/// (FCS's `objExprBindings: OWITH localBindings OEND`). The `with` arrives as
/// `Virtual::With` (`OWITH`, the `WithAsLet` context, distinct from the member
/// form's raw `Token::With`); the binding lands in FCS's `bindings` slot with
/// `SynLeadingKeyword.Synthetic`.
#[test]
fn diff_ast_obj_expr_value_binding() {
    assert_asts_match("let x = { new T() with X = 1 }\n");
}

/// Two value bindings joined by `and` — the head is `Synthetic`, the second is
/// `And` (FCS's `moreLocalBindings`).
#[test]
fn diff_ast_obj_expr_value_binding_and_chain() {
    assert_asts_match("let x = { new T() with X = 1 and Y = 2 }\n");
}

/// A typed value binding `with X : int = 1` — the return-type annotation rides
/// on the binding's `BINDING_RETURN_INFO`, as for a `let`.
#[test]
fn diff_ast_obj_expr_value_binding_typed() {
    assert_asts_match("let x = { new T() with X : int = 1 }\n");
}

/// A function-form value binding `with M () = 1` — a curried head, the same
/// `localBinding` shape as a `let f () = …`.
#[test]
fn diff_ast_obj_expr_value_binding_function_form() {
    assert_asts_match("let x = { new T() with M () = 1 }\n");
}

/// An `inline` value binding — `inline` is accepted on the head (unlike
/// `mutable`); the binding carries `isInline = true`.
#[test]
fn diff_ast_obj_expr_value_binding_inline() {
    assert_asts_match("let x = { new T() with inline X = 1 }\n");
}

/// `inline mutable X = 1` — once `inline` is consumed the parser state accepts a
/// following `mutable`, so FCS takes this (unlike a bare head `mutable`); the
/// binding carries `isInline = true, isMutable = true`.
#[test]
fn diff_ast_obj_expr_value_binding_inline_mutable() {
    assert_asts_match("let x = { new T() with inline mutable X = 1 }\n");
}

/// A `mutable` modifier on a *non-head* (`and`-chained) binding is accepted by
/// FCS (only the bare head `mutable` errors); the second binding carries
/// `isMutable = true`.
#[test]
fn diff_ast_obj_expr_value_binding_nonhead_mutable() {
    assert_asts_match("let x = { new T() with X = 1 and mutable Y = 2 }\n");
}

/// The bare-type base with value bindings (`{ new T with X = 1 }`, no
/// constructor parens) — `argOptions = None` plus the value binding.
#[test]
fn diff_ast_obj_expr_value_binding_bare_type() {
    assert_asts_match("let x = { new T with X = 1 }\n");
}

/// A multi-line value binding indented under the brace.
#[test]
fn diff_ast_obj_expr_value_binding_multiline() {
    assert_asts_match("let x =\n    { new T() with\n        X = 1 }\n");
}

/// A value-binding object expression followed by a sibling declaration — the
/// binding group must close cleanly (consume the `OEND`) and leave the
/// enclosing block's separator for the module loop.
#[test]
fn diff_ast_obj_expr_value_binding_then_sibling() {
    assert_asts_match("let x = { new T() with X = 1 }\nlet y = 1\n");
}

/// A value-binding RHS that is itself a non-trivial expression.
#[test]
fn diff_ast_obj_expr_value_binding_expr_rhs() {
    assert_asts_match("let x = { new T() with X = 1 + 2 }\n");
}

/// A value-binding RHS that is a control-flow expression (`if … then … else …`)
/// — its offside scaffolding (the `=` opens an `OBLOCKBEGIN` block) is drained
/// inside the shared `parse_binding` RHS machinery, so the `OEND`/brace close
/// still land correctly.
#[test]
fn diff_ast_obj_expr_value_binding_if_rhs() {
    assert_asts_match("let x = { new T() with X = if p then 1 else 2 }\n");
}

/// A `match` value-binding RHS — same offside-block concern as the `if` form.
#[test]
fn diff_ast_obj_expr_value_binding_match_rhs() {
    assert_asts_match("let x = { new T() with X = match p with | _ -> 1 }\n");
}

/// A value binding whose RHS is on its own deeper-indented line (`X =⏎ 1`) — the
/// `=` opens a real offside block that `parse_binding` must drain before the
/// brace's `OEND`.
#[test]
fn diff_ast_obj_expr_value_binding_offside_rhs() {
    assert_asts_match("let x =\n    { new T() with\n        X =\n            1 }\n");
}

/// An offside value-binding RHS followed by an `and`-chained binding — the first
/// binding's RHS block must close so the `and` is reached as the next binding.
#[test]
fn diff_ast_obj_expr_value_binding_offside_and_chain() {
    assert_asts_match("let x =\n    { new T() with\n        X = 1\n        and Y = 2 }\n");
}

/// Value bindings *followed by* an extra interface, in proper offside layout
/// (the interface on its own line) — FCS's `objExprBindings opt_OBLOCKSEP
/// objExprInterfaces`. Both slots populate: one `bindings`, one `extraImpls`. The
/// value-binding block's `OEND` close must leave the interface for the handler's
/// `extraImpls` loop.
#[test]
fn diff_ast_obj_expr_value_binding_then_offside_interface() {
    assert_asts_match(
        "let x =\n    { new T() with\n        X = 1\n      interface IDisposable with\n        member _.Dispose () = () }\n",
    );
}

/// A *single-line* value binding immediately followed by `interface …`
/// (`{ new T() with X = 1 interface I with … }`) is FCS's "Unexpected keyword
/// 'interface' in object expression" error — the same offside/layout rule that
/// rejects the second single-line interface (`obj_expr_interface_only_two_single_line_is_lenient`).
/// We don't replicate that layout check, so we accept it losslessly (an `OBJ_EXPR`
/// with the binding and the interface) — a documented leniency, same class as the
/// repeated-separator / single-line-interface ones. The well-formed offside form
/// above is the clean diff; only the missing diagnostic differs here.
#[test]
fn obj_expr_value_binding_single_line_interface_is_lenient() {
    let src =
        "let x = { new T() with X = 1 interface IDisposable with member _.Dispose () = () }\n";
    let parse = parse(src);
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless on the lenient single-line value+interface form",
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == borzoi_cst::syntax::SyntaxKind::OBJ_EXPR),
        "still an object expression",
    );
}

/// Head `mutable` (`{ new T() with mutable X = 1 }`) is FCS's
/// "Unexpected keyword 'mutable' in object expression" error: the LexFilter lexes
/// `with mutable` as the *member* form (raw `Token::With` + `OBLOCKBEGIN`, not the
/// value form's `OWITH`), so it routes to the member branch, where `mutable` is
/// not a valid member. FCS emits the error and drops *all* bindings
/// (`bindings = []`). We mirror the observable shape: still an `OBJ_EXPR`, an
/// error recorded, and no value `BINDING` inside it — a parser-level assertion
/// (an erroring input cannot be a clean diff).
#[test]
fn obj_expr_value_binding_head_mutable_records_error() {
    use borzoi_cst::syntax::SyntaxKind;
    let src = "let x = { new T() with mutable X = 1 }\n";
    let parse = parse(src);
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless for the erroring head-`mutable` form",
    );
    let obj_exprs: Vec<_> = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::OBJ_EXPR)
        .collect();
    assert_eq!(
        obj_exprs.len(),
        1,
        "the head-`mutable` form is still a single object expression",
    );
    // No value binding inside the object expression (FCS's `bindings = []`). The
    // *outer* `let x` is itself a `BINDING`, so scope the check to the OBJ_EXPR's
    // own children rather than all descendants.
    assert!(
        obj_exprs[0]
            .children()
            .all(|c| c.kind() != SyntaxKind::BINDING),
        "FCS drops all bindings on a head `mutable`, so the OBJ_EXPR carries no value binding",
    );
    assert!(
        !parse.errors.is_empty(),
        "the head `mutable` must record a parse error: {:?}",
        parse.errors,
    );
}

/// A *nested* argless `new` in the constructor argument of a value-binding object
/// expression — `{ new Foo(new Bar) with X = 1 }`. FCS rejects the inner `new
/// Bar` ("Unexpected symbol ')'") but still builds the outer `ObjExpr` with the
/// value binding. The inner `new Bar`'s filtered lookahead sees the *outer*
/// `OWITH` past the swallowed `)`, so the base-call span guard must keep it from
/// suppressing its own missing-argument error: the inner `new` is not the brace's
/// head. A parser-level assertion (an erroring input cannot be a clean diff): the
/// outer object expression still carries its value binding, and an error is
/// recorded for the inner `new`.
#[test]
fn obj_expr_value_binding_nested_argless_new_keeps_error() {
    use borzoi_cst::syntax::SyntaxKind;
    let src = "let x = { new Foo(new Bar) with X = 1 }\n";
    let parse = parse(src);
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless on the FCS-erroring nested-argless-new form",
    );
    let obj_value_bindings: usize = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::OBJ_EXPR)
        .map(|o| {
            o.children()
                .filter(|c| c.kind() == SyntaxKind::BINDING)
                .count()
        })
        .sum();
    assert_eq!(
        obj_value_bindings, 1,
        "the outer object expression still carries its value binding",
    );
    assert!(
        !parse.errors.is_empty(),
        "the nested argless `new Bar` must keep its missing-argument error (FCS errors too): {:?}",
        parse.errors,
    );
}

// ---- the bare no-parens form `{ new T }` (FCS `objExpr` alt `NEW atomType`) -

/// The bare `{ new T }` form — `new` followed by a type and then the closing
/// `}`, with no constructor parens, no `with` block and no interfaces. FCS's
/// `objExpr` third alternative (`NEW atomType`), an `ObjExpr` with
/// `argOptions = None` and empty `bindings`/`members`/`extraImpls`. The absence
/// of constructor parens is exactly what makes it an object expression rather
/// than the computation `{ new T() }` (the parenthesised form, regression-tested
/// below).
#[test]
fn diff_ast_bare_new_obj_expr() {
    assert_asts_match("let x = { new T }\n");
}

/// Bare form with a dotted object type — `{ new System.IDisposable }`.
#[test]
fn diff_ast_bare_new_dotted_type() {
    assert_asts_match("let x = { new System.IDisposable }\n");
}

/// Bare form with a dotted *generic* object type — the `<int>` stays inside the
/// `atomType`, and the brace still closes directly after it.
#[test]
fn diff_ast_bare_new_generic_type() {
    assert_asts_match("let x = { new System.Collections.Generic.List<int> }\n");
}

/// Bare form on its own indented line (the brace deeper than the `let`) — the
/// swallowed `}` and the trailing block-closers are recovered the same way as
/// the multi-line member form.
#[test]
fn diff_ast_bare_new_deep_indent() {
    assert_asts_match("let y =\n    { new T }\n");
}

/// A bare object expression followed by a sibling declaration — the object
/// expression must close cleanly and leave the enclosing block's separator for
/// the module loop (the Stage A sibling discipline, on the no-member tail).
#[test]
fn diff_ast_bare_new_then_sibling() {
    assert_asts_match("let x = { new T }\nlet y = 1\n");
}

/// A bare object expression as a function argument — the whole `{ new T }` is
/// one atom.
#[test]
fn diff_ast_bare_new_as_arg() {
    assert_asts_match("let x = ignore { new T }\n");
}

// ---- a bare object expression nested directly before an outer with/interface

/// A bare object expression `{ new Bar }` as the *constructor argument* of an
/// outer object expression that then has its own `with member …` block:
/// `{ new Foo({ new Bar }) with member _.M = () }`. The inner `}` and the
/// enclosing `)` are LexFilter-swallowed, so the inner brace's filtered
/// lookahead sees the *outer* `with`; the bare-form detection (and the handler's
/// with-emission) must use the raw stream's inner `}` so the inner stays a bare
/// `OBJ_EXPR` and the outer `with` is not stolen. FCS produces two nested
/// `ObjExpr`s.
#[test]
fn diff_ast_bare_new_nested_before_outer_with() {
    assert_asts_match("let x = { new Foo({ new Bar }) with member _.M = () }\n");
}

/// As above but the outer continuation is an `interface` rather than a `with`
/// block (`{ new Foo({ new Bar }) interface IA with member _.A () = () }`) — the
/// same swallowed-closer lookahead hazard, guarded the same way.
#[test]
fn diff_ast_bare_new_nested_before_outer_interface() {
    assert_asts_match("let x = { new Foo({ new Bar }) interface IA with member _.A () = () }\n");
}

// ---- regression: a `new`-headed brace with NO `with` stays a CE ----------

/// `{ new T(1, 2) }` — args but no `with`/interface, so it is a computation
/// expression wrapping a `SynExpr.New`, *not* an object expression.
#[test]
fn diff_ast_new_in_brace_is_computation() {
    assert_asts_match("let x = { new T(1, 2) }\n");
}

/// `{ new T() }` — unit args, no `with`: also a `ComputationExpr(New(T, Const
/// Unit))`. The parens are the sole discriminator from the bare `{ new T }`
/// object expression above.
#[test]
fn diff_ast_new_unit_in_brace_is_computation() {
    assert_asts_match("let x = { new System.Object() }\n");
}

// ---- deferred forms recover without corruption ---------------------------

/// The bare `as base` keyword form (`{ new C() as base with … }`) is an object
/// expression in FCS too, but FCS reports FS0564 (the `AS BASE` production
/// always errors — only a quoted `` ``base`` `` is valid). We mirror that: the
/// object expression is still parsed (an `OBJ_EXPR` with the member), but the
/// base alias records the same error, so this is a parser-level assertion rather
/// than a clean diff.
#[test]
fn obj_expr_bare_base_keyword_records_error_but_parses() {
    let src = "let x = { new System.Object() as base with member x.M () = 1 }\n";
    let parse = parse(src);
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless for the erroring `as base` form",
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == borzoi_cst::syntax::SyntaxKind::OBJ_EXPR),
        "the `as base` form is still an object expression, not a computation expression",
    );
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("'as' bindings")),
        "the bare `as base` keyword must record FCS's FS0564-equivalent error: {:?}",
        parse.errors,
    );
}

// ---- base-span specificity: a trailing/nested argless `new` is not the base -

/// `{ new Foo(new Bar) }` — the *base* call `new Foo(…)` has constructor parens,
/// so the brace is a computation, not the bare object expression. The inner
/// `new Bar` is argless (FCS rejects it: "Unexpected symbol ')'"), but it is not
/// the brace's head `new`, so the bare-form detector (keyed on the *base* `new`
/// keyword's span) must not misfire and wrap the whole brace as an `OBJ_EXPR`.
/// FCS itself errors here, so this is a parser-level assertion: lossless, and a
/// computation rather than a (spurious) bare object expression.
#[test]
fn bare_new_base_span_ignores_nested_argless_new() {
    let src = "let x = { new Foo(new Bar) }\n";
    let parse = parse(src);
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless on the FCS-erroring nested-argless-new form",
    );
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == borzoi_cst::syntax::SyntaxKind::OBJ_EXPR),
        "the parenthesised base call is a computation, not a bare object expression",
    );
}

/// `{ new T(); new U }` — a sequence whose *base* call `new T()` has parens (a
/// computation) but whose trailing `new U` is argless and sits right before the
/// `}`. FCS rejects the trailing `new U` ("Unexpected symbol '}'"); our
/// base-span detector must not mistake that trailing argless `new` for the
/// brace's head and emit a bare `OBJ_EXPR`. Lossless, and a computation.
#[test]
fn bare_new_base_span_ignores_trailing_argless_new() {
    let src = "let x = { new T(); new U }\n";
    let parse = parse(src);
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless on the FCS-erroring trailing-argless-new form",
    );
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == borzoi_cst::syntax::SyntaxKind::OBJ_EXPR),
        "the sequence is a computation, not a bare object expression",
    );
}
