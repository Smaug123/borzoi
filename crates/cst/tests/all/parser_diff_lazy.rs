//! Differential test (`parser::parse` vs FCS): the `declExpr`-level keyword
//! prefixes `lazy e` / `assert e` — FCS's `SynExpr.Lazy(expr, range)` /
//! `SynExpr.Assert(expr, range)` (the productions `LAZY declExpr %prec
//! expr_lazy` / `ASSERT declExpr %prec expr_assert`, `pars.fsy:4346`/`:4349`).
//!
//! Both sit at FCS's `expr_app` precedence (the application level, tighter than
//! every infix operator), but their operand is grammatically a full `declExpr`.
//! Precedence clips that operand to exactly this codebase's `parse_minus_expr`
//! level *plus* a leading open-lower range:
//!
//!  * application / postfix / unary minus are absorbed (`lazy f y` =
//!    `Lazy(App(f, y))`, `lazy -y` = `Lazy(-y)`, `lazy a.b` = `Lazy(a.b)`);
//!  * control-flow keywords are absorbed (`lazy if … ` = `Lazy(IfThenElse …)`);
//!  * a *leading* open-lower range is absorbed by `lazy` (`lazy ..3` =
//!    `Lazy(IndexRange(None, 3))`; FCS rejects `assert ..3`);
//!  * but infix / cons / tuple / range-with-lhs bind *looser* (`lazy a + b` =
//!    `(lazy a) + b`, `lazy a :: b` = `(lazy a) :: b`, `lazy a, b` =
//!    `(lazy a), b`, `lazy a .. b` = `(lazy a) .. b`).
//!
//! So the parser dispatches `lazy`/`assert` beside `upcast`/`downcast`/`new` in
//! `parse_minus_expr` and parses the operand recursively at that level. This
//! mirrors the `upcast`/`downcast` slice (`parser_diff_coercion.rs`).

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, Expr, SyntaxKind};

// ---- the basic prefix forms ---------------------------------------------

/// `lazy 1` — `Lazy(Const 1)`, the motivating shape.
#[test]
fn diff_ast_lazy_const() {
    assert_asts_match("let x = lazy 1\n");
}

/// `assert true` — `Assert(Const true)`.
#[test]
fn diff_ast_assert_bool() {
    assert_asts_match("let x = assert true\n");
}

/// `lazy y` — `Lazy(Ident "y")`.
#[test]
fn diff_ast_lazy_ident() {
    assert_asts_match("let x = lazy y\n");
}

// ---- the operand absorbs application / postfix / unary minus -------------

/// `lazy f y` — the operand is the *whole* application: `Lazy(App(f, y))`.
#[test]
fn diff_ast_lazy_app() {
    assert_asts_match("let x = lazy f y\n");
}

/// `assert f y` — the same for `assert`: `Assert(App(f, y))`.
#[test]
fn diff_ast_assert_app() {
    assert_asts_match("let x = assert f y\n");
}

/// `lazy -y` — the unary-minus prefix is part of the operand: `Lazy(-y)`.
/// (`-y` is the non-sign-folded `App(op_UnaryNegation, y)` shape.)
#[test]
fn diff_ast_lazy_unary_minus() {
    assert_asts_match("let x = lazy -y\n");
}

/// `lazy a.b` — postfix dot is part of the operand: `Lazy(LongIdent a.b)`.
#[test]
fn diff_ast_lazy_dot_get() {
    assert_asts_match("let x = lazy a.b\n");
}

/// `lazy a.[0]` — postfix index is part of the operand:
/// `Lazy(DotIndexedGet(a, 0))`.
#[test]
fn diff_ast_lazy_dot_indexed() {
    assert_asts_match("let x = lazy a.[0]\n");
}

/// A parenthesised operand pins a whole application/tuple under the `lazy`.
#[test]
fn diff_ast_lazy_paren() {
    assert_asts_match("let x = lazy (f x)\n");
}

/// A *bare* (unparenthesised) `let … in` as `lazy`'s operand — `lazy let y = 1
/// in y` is `Lazy(LetOrUse([y = 1], y))`. The non-block `let … in` (a raw
/// `Token::Let`) is now dispatched as a `minusExpr`-level operand, so the
/// prefix-keyword operand absorbs it without parentheses (previously a
/// documented reject shared with `assert`/`fixed`).
#[test]
fn diff_ast_lazy_bare_let_in() {
    assert_asts_match("let x = lazy let y = 1 in y\n");
}

// ---- infix / cons / tuple / range bind looser ---------------------------

/// `lazy 1 + 2` — `lazy` binds tighter than `+`: `App(+, Lazy 1, 2)`.
#[test]
fn diff_ast_lazy_then_infix() {
    assert_asts_match("let x = lazy 1 + 2\n");
}

/// `lazy a :: b` — `(lazy a) :: b` (cons binds looser).
#[test]
fn diff_ast_lazy_then_cons() {
    assert_asts_match("let x = lazy a :: b\n");
}

/// `lazy a |> b` — `(lazy a) |> b` (pipe binds looser).
#[test]
fn diff_ast_lazy_piped() {
    assert_asts_match("let x = lazy a |> b\n");
}

/// `lazy a .. b` — `(lazy a) .. b` (a range *with a left bound* binds looser).
#[test]
fn diff_ast_lazy_then_range() {
    assert_asts_match("let x = lazy a .. b\n");
}

// ---- the operand absorbs a trailing `<-` assignment ----------------------

/// `lazy a <- b` — the `<-` assignment binds *inside* the operand:
/// `Lazy(LongIdentSet(a, b))`, not `Set(Lazy a, b)`. FCS's `LAZY declExpr`
/// operand is a full `declExpr`, which includes `minusExpr LARROW
/// declExprBlock`; precedence keeps `+`/`::`/`,`/`:>`/`:=` *out* of the operand
/// but folds `<-` *in* (the `declExpr: minusExpr` reduction has no precedence,
/// so yacc shifts the `<-`).
#[test]
fn diff_ast_lazy_assign_operand() {
    assert_asts_match("let x = lazy a <- b\n");
}

/// `assert a <- b` — the same `<-` folding for `assert`:
/// `Assert(LongIdentSet(a, b))`.
#[test]
fn diff_ast_assert_assign_operand() {
    assert_asts_match("let x = assert a <- b\n");
}

/// `lazy arr.[i] <- v` — the operand is a `DotIndexedSet`, the indexer-target
/// assignment form: `Lazy(DotIndexedSet(arr, i, v))`.
#[test]
fn diff_ast_lazy_dot_indexed_set_operand() {
    assert_asts_match("let x = lazy arr.[i] <- v\n");
}

// ---- but the type-relation / `:=` continuations stay *out* of the operand -

/// `lazy a :> T` — the upcast operator binds looser than `lazy`:
/// `Upcast(Lazy a, T)`, i.e. `(lazy a) :> T`. (Guards against the `<-` fix
/// over-folding: only `<-`, not the type-relation ops, joins the operand.)
#[test]
fn diff_ast_lazy_then_upcast() {
    assert_asts_match("let x = lazy a :> T\n");
}

/// `lazy a := b` — the ref-cell assignment binds looser than `lazy`:
/// `App(App(:=, Lazy a), b)`, i.e. `(lazy a) := b`.
#[test]
fn diff_ast_lazy_then_colon_equals() {
    assert_asts_match("let x = lazy a := b\n");
}

// ---- the operand absorbs control-flow keywords --------------------------

/// `lazy if p then q else r` — the operand is a full `declExpr`, so it absorbs
/// the whole `if`: `Lazy(IfThenElse(p, q, r))`.
#[test]
fn diff_ast_lazy_if() {
    assert_asts_match("let x = lazy if p then q else r\n");
}

/// `lazy match …` — likewise absorbs a `match`.
#[test]
fn diff_ast_lazy_match() {
    assert_asts_match("let x = lazy match y with _ -> 0\n");
}

// ---- the operand absorbs a leading open-lower range ----------------------

/// `lazy ..3` — the one place the `declExpr` operand exceeds `minusExpr`: a
/// *leading* open-lower range is absorbed, `Lazy(IndexRange(None, 3))`. FCS
/// accepts this cleanly (it rejects `upcast ..3`, whose operand is `minusExpr`).
#[test]
fn diff_ast_lazy_open_lower_range() {
    assert_asts_match("let x = lazy ..3\n");
}

/// `lazy ..a .. b` — the leading open-lower range's upper is itself a range, so
/// the whole `IndexRange(None, IndexRange(a, b))` stays inside the `Lazy`.
#[test]
fn diff_ast_lazy_chained_open_lower_range() {
    assert_asts_match("let x = lazy ..a .. b\n");
}

// ---- nesting ------------------------------------------------------------

/// `lazy lazy z` — chains right: `Lazy(Lazy z)`.
#[test]
fn diff_ast_lazy_chained() {
    assert_asts_match("let x = lazy lazy z\n");
}

// ---- positions ----------------------------------------------------------

/// In a tuple element (each element is a fresh expression-start position).
#[test]
fn diff_ast_lazy_in_tuple() {
    assert_asts_match("let x = (lazy a, assert b)\n");
}

/// As a function argument (must be parenthesised — a `declExpr` is not an
/// `atomicExpr`, so `f lazy x` would not nest the `lazy` under the arg).
#[test]
fn diff_ast_lazy_as_arg() {
    assert_asts_match("let x = f (lazy y)\n");
}

/// As an infix RHS: `a + lazy b` is `App(+, a, Lazy b)` — the `lazy` is reached
/// through the operator's recursive RHS, which descends to `parse_minus_expr`.
#[test]
fn diff_ast_lazy_as_infix_rhs() {
    assert_asts_match("let x = a + lazy b\n");
}

/// As a cons RHS: `a :: lazy b` is `a :: (lazy b)`.
#[test]
fn diff_ast_lazy_as_cons_rhs() {
    assert_asts_match("let x = a :: lazy b\n");
}

// ---- green-tree shape (no FCS) ------------------------------------------

/// The node wraps the keyword token then the operand expr:
/// `LAZY_EXPR > [LAZY_TOK, <inner-expr>]`.
#[test]
fn green_tree_lazy_shape() {
    let parse = parse("let x = lazy y\n");
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LAZY_EXPR)
        .expect("expected a LAZY_EXPR node");
    assert!(
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::LAZY_TOK),
        "LAZY_EXPR must carry a LAZY_TOK",
    );
    assert!(
        node.children().any(|c| Expr::can_cast(c.kind())),
        "LAZY_EXPR must contain a structured operand expr",
    );
}

/// `assert true` builds an `ASSERT_EXPR` carrying an `ASSERT_TOK`.
#[test]
fn green_tree_assert_shape() {
    let parse = parse("let x = assert true\n");
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ASSERT_EXPR)
        .expect("expected an ASSERT_EXPR node");
    assert!(
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::ASSERT_TOK),
        "ASSERT_EXPR must carry an ASSERT_TOK",
    );
}

// ---- error recovery -----------------------------------------------------

/// A bare `lazy` with no operand — like the `upcast`/`new` recovery paths, the
/// parser records the missing-operand error, still emits the `LAZY_EXPR`
/// (carrying just the keyword), and stays lossless (never panics). FCS also
/// reports a parse error here.
#[test]
fn lazy_missing_operand_recovers_without_panic() {
    let src = "let x = lazy\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for the operandless `{src}`",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless even on the recovery path",
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LAZY_EXPR),
        "the operandless recovery must still emit a LAZY_EXPR",
    );
}

/// `assert ..3` is *not* the same as `lazy ..3`: FCS reports FS0590
/// ("'assert' may not be used as a first class value") rather than accepting a
/// leading open-lower range as the operand. We only pin the parser-side
/// contract here: no silent clean parse and no losslessness break.
#[test]
fn assert_open_lower_range_recovers_without_panic() {
    for src in ["let x = assert ..3\n", "let x = assert ..a .. b\n"] {
        let parse = parse(src);
        assert!(
            !parse.errors.is_empty(),
            "expected a parse error for `{src}`",
        );
        assert_eq!(
            parse.root.text().to_string(),
            src,
            "round-trip must be lossless even on the recovery path",
        );
        assert!(
            parse
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::ASSERT_EXPR),
            "the recovery must still emit an ASSERT_EXPR",
        );
    }
}

/// `- lazy y` — FCS rejects a `declExpr` keyword as a `minusExpr`-prefix
/// operand (the `minusExpr` operand grammar is `minusExpr`, not `declExpr`),
/// so the parser records the prefix-keyword diagnostic while still building a
/// lossless tree (the `LAZY_EXPR` nested under the prefix `APP_EXPR`). It must
/// never panic on the recursive-operand path.
#[test]
fn lazy_after_prefix_op_recovers_without_panic() {
    let src = "let x = - lazy y\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "expected a prefix-keyword parse error for `{src}`",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless even on the recovery path",
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LAZY_EXPR),
        "the recovery must still emit a LAZY_EXPR",
    );
}

/// The *relabelled* offside/control-flow `lazy`/`assert` (LexFilter's
/// `OLAZY`/`OASSERT`) must also trip the prefix-keyword diagnostic — the
/// operand-block relabel does not exempt it from FCS's `declExpr`-not-`minusExpr`
/// rejection of `- lazy …` / `& assert …`. Guards the `maybe_warn_keyword_after_prefix`
/// virtual arms (else the relabelled form would slip through with no error).
#[test]
fn virtual_lazy_assert_after_prefix_op_still_errors() {
    for (src, kind) in [
        ("let x = - lazy if c then a else b\n", SyntaxKind::LAZY_EXPR),
        ("let x = & assert\n    ok\n", SyntaxKind::ASSERT_EXPR),
    ] {
        let parse = parse(src);
        assert!(
            !parse.errors.is_empty(),
            "{src:?}: a relabelled `lazy`/`assert` after a prefix operator must still error",
        );
        assert_eq!(
            parse.root.text().to_string(),
            src,
            "{src:?}: recovery must stay lossless",
        );
        assert!(
            parse.root.descendants().any(|n| n.kind() == kind),
            "{src:?}: recovery must still build the {kind:?}",
        );
    }
}

/// `(lazy) y` — `lazy` is the last token *inside* the parens, so LexFilter has
/// swallowed the closing `)`. The operand must not be parsed across that
/// swallowed closer: the `lazy` has no operand (the `)` belongs to the paren),
/// so the `LAZY_EXPR` carries just its keyword, the paren claims its `)`, and
/// the trailing `y` applies *outside*. Without the swallowed-closer guard the
/// `)` would be dragged into the operand as an `ERROR` and `y` mis-nested under
/// the `lazy`. Recovery only (invalid F#): assert lossless, errored, no panic,
/// and that the `LAZY_EXPR` got no structured operand.
#[test]
fn lazy_operand_stops_at_swallowed_closer() {
    let src = "let x = (lazy) y\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for the operandless `{src}`",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless even on the recovery path",
    );
    let lazy = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::LAZY_EXPR)
        .expect("expected a LAZY_EXPR node");
    assert!(
        !lazy.children().any(|c| Expr::can_cast(c.kind())),
        "the swallowed `)` must not be parsed as the `lazy` operand; \
         LAZY_EXPR should carry no structured operand, got:\n{lazy:#?}",
    );
}

/// A bare `assert` with no operand recovers the same way.
#[test]
fn assert_missing_operand_recovers_without_panic() {
    let src = "let x = assert\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for the operandless `{src}`",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless even on the recovery path",
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::ASSERT_EXPR),
        "the operandless recovery must still emit an ASSERT_EXPR",
    );
}

// ---- offside-block operand (`lazy`/`assert` at end of line) --------------
//
// When the operand starts on a *different line* (or is a control-flow keyword),
// FCS's LexFilter rewrites `LAZY`/`ASSERT` to `OLAZY`/`OASSERT` and pushes a
// `CtxtSeqBlock` (`LexFilter.fs:2232`, guarded by `isControlFlowOrNotSameLine`),
// so the whole indented offside block is the operand — infix continuations
// included. Thus `lazy⏎ a⏎ |> b` is `Lazy(a |> b)`, not the single-line
// `(lazy a) |> b`. Found via the corpus divergence sweep (`ListProperties.fs`).

/// `lazy` at end of line, operand block spanning a `|>` continuation.
#[test]
fn diff_ast_lazy_offside_block_piped() {
    assert_asts_match("let x =\n    lazy\n        a\n        |> b\n");
}

/// `assert` behaves identically (same LexFilter arm).
#[test]
fn diff_ast_assert_offside_block_piped() {
    assert_asts_match("let x =\n    assert\n        a\n        |> b\n");
}

/// The motivating corpus shape: `==> (lazy⏎ … |> …)` inside parens.
#[test]
fn diff_ast_lazy_offside_block_in_parens() {
    assert_asts_match("let x = q ==> (lazy\n        a\n        |> b)\n");
}

/// A two-statement operand block: `lazy⏎ f x⏎ g y` is `Lazy(Sequential(f x, g y))`.
#[test]
fn diff_ast_lazy_offside_block_sequential() {
    assert_asts_match("let x =\n    lazy\n        f a\n        g b\n");
}

/// An offside/control-flow `lazy` as an *infix RHS* — the `is_expr_start_at`
/// gate (the infix/cons RHS starter) must admit the `OLAZY`/`OASSERT` virtual,
/// symmetric with `peek_is_expr_start`. `a ||⏎ lazy⏎ b` = `a || lazy b`.
#[test]
fn diff_ast_lazy_offside_block_as_infix_rhs() {
    assert_asts_match("let x =\n    a ||\n    lazy\n        b\n");
}

/// Same-line control-flow `lazy` as an infix RHS: `a || lazy if c then d else e`.
#[test]
fn diff_ast_lazy_control_flow_as_infix_rhs() {
    assert_asts_match("let x = a || lazy if c then d else e\n");
}
