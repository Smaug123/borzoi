//! Differential test (`parser::parse` vs FCS): the `fixed e` pinning prefix —
//! FCS's `SynExpr.Fixed(expr, range)` (the production `FIXED declExpr`,
//! `pars.fsy:4624`).
//!
//! `fixed` looks like `lazy`/`assert` — a keyword prefix taking a `declExpr` —
//! but binds the **opposite** way. `LAZY/ASSERT declExpr %prec expr_lazy` clip
//! their operand tight (`expr_app` precedence, so `lazy a + b` = `(lazy a) + b`).
//! `FIXED declExpr` carries **no `%prec`**, so the rule inherits its rightmost
//! terminal's precedence (`FIXED`, which has none) and every shift/reduce
//! conflict defaults to *shift*: the operand greedily absorbs the whole
//! `declExpr`. Verified against FCS:
//!
//!  * application / postfix / unary minus (`fixed f x` = `Fixed(App(f, x))`,
//!    `fixed -y`, `fixed a.b`);
//!  * **every** infix / cons / pipe (`fixed a + b` = `Fixed(a + b)`,
//!    `fixed a :: b` = `Fixed(a :: b)`, `fixed a |> b`) — looser than `fixed`,
//!    unlike `lazy`;
//!  * tuple `,` (`fixed a, b` = `Fixed(Tuple(a, b))`), `:=`, `<-`;
//!  * type-relation `:>` / `:?` (`fixed a :> T` = `Fixed(Upcast(a, T))`);
//!  * a leading open-lower range (`fixed ..3` = `Fixed(IndexRange(None, 3))`);
//!  * control-flow (`fixed if …`, `fixed match …`, `fixed fun …`,
//!    `fixed let … in …`).
//!
//! Only three forms bind *looser* and stay outside the operand — they sit above
//! `declExpr` in FCS's grammar: the `: T` type annotation
//! (`typedSequentialExpr`, so `fixed a : T` = `Typed(Fixed a, T)`), `;`
//! sequencing (`sequentialExpr`), and `in`. So the parser dispatches `fixed`
//! beside `lazy`/`assert` in `parse_minus_expr` but parses the operand with the
//! full `parse_expr` (== FCS `declExpr`) rather than the tight Pratt frame.
//!
//! `fixed` is only *semantically* valid as a `use x = fixed e` binding RHS, but
//! that restriction is a typecheck error, not a parse error — FCS builds the
//! `SynExpr.Fixed` node anywhere a `declExpr` appears, so these tests use the
//! `let x = fixed …` RHS (a clean parse on both sides; a top-level `use` would
//! draw FCS's "'use' bindings are not permitted in modules" diagnostic).

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, Expr, SyntaxKind};

// ---- the basic prefix forms ---------------------------------------------

/// `fixed arr` — `Fixed(Ident "arr")`, the motivating shape (`use p = fixed arr`).
#[test]
fn diff_ast_fixed_ident() {
    assert_asts_match("let x = fixed arr\n");
}

/// `fixed 1` — `Fixed(Const 1)`.
#[test]
fn diff_ast_fixed_const() {
    assert_asts_match("let x = fixed 1\n");
}

// ---- the operand absorbs application / postfix / unary minus -------------

/// `fixed f x` — the operand is the *whole* application: `Fixed(App(f, x))`.
#[test]
fn diff_ast_fixed_app() {
    assert_asts_match("let x = fixed f x\n");
}

/// `fixed -y` — the unary-minus prefix is part of the operand: `Fixed(-y)`.
#[test]
fn diff_ast_fixed_unary_minus() {
    assert_asts_match("let x = fixed -y\n");
}

/// `fixed a.b` — postfix dot is part of the operand: `Fixed(LongIdent a.b)`.
#[test]
fn diff_ast_fixed_dot_get() {
    assert_asts_match("let x = fixed a.b\n");
}

/// `fixed a.[0]` — postfix index is part of the operand:
/// `Fixed(DotIndexedGet(a, 0))`.
#[test]
fn diff_ast_fixed_dot_indexed() {
    assert_asts_match("let x = fixed a.[0]\n");
}

/// `fixed &arr.[0]` — the idiomatic "address of an array element" operand:
/// `Fixed(AddressOf(DotIndexedGet(arr, 0)))`. The `&` prefix is part of the
/// operand.
#[test]
fn diff_ast_fixed_address_of_element() {
    assert_asts_match("let x = fixed &arr.[0]\n");
}

/// A parenthesised operand pins a whole application/tuple under the `fixed`.
#[test]
fn diff_ast_fixed_paren() {
    assert_asts_match("let x = fixed (f x)\n");
}

// ---- the operand absorbs *every* infix / cons / pipe (looser than lazy) ---

/// `fixed a + b` — unlike `lazy`, `fixed` binds *looser* than `+`, so the `+`
/// folds into the operand: `Fixed(App(+, a, b))`. (The crux: `FIXED declExpr`
/// has no `%prec`, so the conflict shifts.)
#[test]
fn diff_ast_fixed_then_infix() {
    assert_asts_match("let x = fixed a + b\n");
}

/// `fixed a * b + c` — a whole infix tree folds in: `Fixed((a * b) + c)`.
#[test]
fn diff_ast_fixed_infix_tree() {
    assert_asts_match("let x = fixed a * b + c\n");
}

/// `fixed a :: b` — cons folds into the operand: `Fixed(a :: b)`.
#[test]
fn diff_ast_fixed_then_cons() {
    assert_asts_match("let x = fixed a :: b\n");
}

/// `fixed a |> b` — the pipe folds into the operand: `Fixed(a |> b)`.
#[test]
fn diff_ast_fixed_piped() {
    assert_asts_match("let x = fixed a |> b\n");
}

/// `fixed a .. b` — a left-bounded range folds into the operand:
/// `Fixed(IndexRange(a, b))`.
#[test]
fn diff_ast_fixed_then_range() {
    assert_asts_match("let x = fixed a .. b\n");
}

// ---- the operand absorbs the tuple comma ---------------------------------

/// `fixed a, b` — the comma folds into the operand: `Fixed(Tuple(a, b))`.
/// (For `lazy` it binds looser: `lazy a, b` = `(lazy a), b`.)
#[test]
fn diff_ast_fixed_then_tuple() {
    assert_asts_match("let x = fixed a, b\n");
}

/// `fixed a, b, c` — a 3-tuple folds in: `Fixed(Tuple(a, b, c))`.
#[test]
fn diff_ast_fixed_three_tuple() {
    assert_asts_match("let x = fixed a, b, c\n");
}

// ---- the operand absorbs `:=` and `<-` -----------------------------------

/// `fixed a := b` — ref-cell assignment folds into the operand:
/// `Fixed(App(App(:=, a), b))`.
#[test]
fn diff_ast_fixed_colon_equals() {
    assert_asts_match("let x = fixed a := b\n");
}

/// `fixed a <- b` — the `<-` assignment folds into the operand:
/// `Fixed(LongIdentSet(a, b))`.
#[test]
fn diff_ast_fixed_assign_operand() {
    assert_asts_match("let x = fixed a <- b\n");
}

/// `fixed arr.[i] <- v` — the operand is a `DotIndexedSet`:
/// `Fixed(DotIndexedSet(arr, i, v))`.
#[test]
fn diff_ast_fixed_dot_indexed_set_operand() {
    assert_asts_match("let x = fixed arr.[i] <- v\n");
}

// ---- the operand absorbs the type-relation operators ---------------------

/// `fixed a :> T` — the upcast folds into the operand: `Fixed(Upcast(a, T))`.
/// (For `lazy` it binds looser: `lazy a :> T` = `(lazy a) :> T`.)
#[test]
fn diff_ast_fixed_then_upcast() {
    assert_asts_match("let x = fixed a :> T\n");
}

/// `fixed a :? T` — the type test folds in: `Fixed(TypeTest(a, T))`.
#[test]
fn diff_ast_fixed_then_typetest() {
    assert_asts_match("let x = fixed a :? T\n");
}

// ---- but `: T` type annotation binds *looser* (stays outside) ------------

/// `fixed a : T` — the `: T` type annotation sits above `declExpr` in FCS's
/// grammar (`typedSequentialExpr`), so it binds *looser* than `fixed`:
/// `Typed(Fixed(a), T)`, not `Fixed(Typed(a, T))`.
#[test]
fn diff_ast_fixed_then_type_annotation() {
    assert_asts_match("let x = fixed a : T\n");
}

// ---- the operand absorbs control-flow keywords --------------------------

/// `fixed if p then q else r` — the operand is a full `declExpr`, so it absorbs
/// the whole `if`: `Fixed(IfThenElse(p, q, r))`.
#[test]
fn diff_ast_fixed_if() {
    assert_asts_match("let x = fixed if p then q else r\n");
}

/// `fixed match …` — absorbs a `match`.
#[test]
fn diff_ast_fixed_match() {
    assert_asts_match("let x = fixed match y with _ -> 0\n");
}

/// `fixed fun x -> x` — absorbs a lambda: `Fixed(Lambda …)`.
#[test]
fn diff_ast_fixed_fun() {
    assert_asts_match("let x = fixed fun y -> y\n");
}

/// `fixed (let y = 1 in y)` — a parenthesised `let … in` operand:
/// `Fixed(Paren(LetOrUse …))`.
#[test]
fn diff_ast_fixed_paren_let_in() {
    assert_asts_match("let x = fixed (let y = 1 in y)\n");
}

/// The *bare* nested form `fixed let y = 1 in y` — `Fixed(LetOrUse([y = 1], y))`.
/// A non-block inline `let … in` now dispatches as a `minusExpr`-level operand
/// (a raw `Token::Let` in `parse_minus_expr`), so the prefix-keyword operand
/// absorbs it without parentheses (previously a documented reject shared with
/// `lazy`/`assert`).
#[test]
fn diff_ast_fixed_bare_let_in() {
    assert_asts_match("let x = fixed let y = 1 in y\n");
}

// ---- the operand absorbs a leading open-lower range ----------------------

/// `fixed ..3` — a *leading* open-lower range is part of the operand:
/// `Fixed(IndexRange(None, 3))`.
#[test]
fn diff_ast_fixed_open_lower_range() {
    assert_asts_match("let x = fixed ..3\n");
}

// ---- nesting ------------------------------------------------------------

/// `fixed fixed z` — chains right: `Fixed(Fixed z)`.
#[test]
fn diff_ast_fixed_chained() {
    assert_asts_match("let x = fixed fixed z\n");
}

/// `fixed lazy z` — `fixed`'s operand absorbs the whole `lazy`:
/// `Fixed(Lazy z)`.
#[test]
fn diff_ast_fixed_of_lazy() {
    assert_asts_match("let x = fixed lazy z\n");
}

// ---- positions ----------------------------------------------------------

/// As a function argument (must be parenthesised — a `declExpr` is not an
/// `atomicExpr`, so `f fixed x` would be an error; `f (fixed x)` nests it).
#[test]
fn diff_ast_fixed_as_arg() {
    assert_asts_match("let x = f (fixed y)\n");
}

/// As an infix RHS: `a + fixed b` is `App(+, a, Fixed b)` — the `fixed` is
/// reached through the operator's recursive RHS (which descends to
/// `parse_minus_expr`).
#[test]
fn diff_ast_fixed_as_infix_rhs() {
    assert_asts_match("let x = a + fixed b\n");
}

/// In a list element followed by `;` — the `;` sequences *outside* the operand,
/// so `[ fixed a; fixed b ]` is two separate `Fixed`s.
#[test]
fn diff_ast_fixed_in_list_sequence() {
    assert_asts_match("let x = [ fixed a; fixed b ]\n");
}

/// In a `use` binding inside a function body — the idiomatic, *semantically
/// valid* position. Parenthesised `use … in …` keeps it a clean parse.
#[test]
fn diff_ast_fixed_in_use_binding() {
    assert_asts_match("let f () = (use p = fixed arr in p)\n");
}

// ---- multi-line offside operand -----------------------------------------

/// The operand may sit on a following line when indented past the binding —
/// the general offside rules govern it (`fixed` has no special offside token).
#[test]
fn diff_ast_fixed_operand_next_line() {
    assert_asts_match("let x =\n    fixed arr\n");
}

// ---- green-tree shape (no FCS) ------------------------------------------

/// The node wraps the keyword token then the operand expr:
/// `FIXED_EXPR > [FIXED_TOK, <inner-expr>]`.
#[test]
fn green_tree_fixed_shape() {
    let parse = parse("let x = fixed y\n");
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    let node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::FIXED_EXPR)
        .expect("expected a FIXED_EXPR node");
    assert!(
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::FIXED_TOK),
        "FIXED_EXPR must carry a FIXED_TOK",
    );
    assert!(
        node.children().any(|c| Expr::can_cast(c.kind())),
        "FIXED_EXPR must contain a structured operand expr",
    );
}

// ---- error recovery -----------------------------------------------------

/// A bare `fixed` with no operand — like the `lazy`/`upcast` recovery paths,
/// the parser records the missing-operand error, still emits the `FIXED_EXPR`
/// (carrying just the keyword), and stays lossless (never panics). FCS also
/// reports a parse error here.
#[test]
fn fixed_missing_operand_recovers_without_panic() {
    let src = "let x = fixed\n";
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
            .any(|n| n.kind() == SyntaxKind::FIXED_EXPR),
        "the operandless recovery must still emit a FIXED_EXPR",
    );
}

/// `- fixed y` — FCS rejects a `declExpr` keyword as a `minusExpr`-prefix
/// operand, so the parser records the prefix-keyword diagnostic while still
/// building a lossless tree (the `FIXED_EXPR` nested under the prefix
/// `APP_EXPR`). It must never panic on the recursive-operand path.
#[test]
fn fixed_after_prefix_op_recovers_without_panic() {
    let src = "let x = - fixed y\n";
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
            .any(|n| n.kind() == SyntaxKind::FIXED_EXPR),
        "the recovery must still emit a FIXED_EXPR",
    );
}

/// `(fixed) y` — `fixed` is the last token *inside* the parens, so LexFilter has
/// swallowed the closing `)`. The operand must not be parsed across that
/// swallowed closer: the `fixed` has no operand (the `)` belongs to the paren),
/// so the `FIXED_EXPR` carries just its keyword, the paren claims its `)`, and
/// the trailing `y` applies *outside*. Recovery only (invalid F#): assert
/// lossless, errored, no panic, and that the `FIXED_EXPR` got no structured
/// operand.
#[test]
fn fixed_operand_stops_at_swallowed_closer() {
    let src = "let x = (fixed) y\n";
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
    let fixed = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::FIXED_EXPR)
        .expect("expected a FIXED_EXPR node");
    assert!(
        !fixed.children().any(|c| Expr::can_cast(c.kind())),
        "the swallowed `)` must not be parsed as the `fixed` operand; \
         FIXED_EXPR should carry no structured operand, got:\n{fixed:#?}",
    );
}

/// `f fixed x` — `fixed` is a `declExpr`, not an `atomicExpr`, so it cannot be
/// an application argument; FCS reports a parse error (and recovers with a
/// `SynExpr.FromParseError` node, which the diff normaliser deliberately does
/// not project — so this is a our-side recovery check, not a differential one).
/// Our parser must error and stay lossless without panicking.
#[test]
fn fixed_as_app_arg_recovers_without_panic() {
    let src = "let x = f fixed x\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error for `fixed` as an application arg in `{src}`",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless even on the recovery path",
    );
}

/// `fixed a :> T <- v` — a completed `FIXED_EXPR` is a `declExpr`, **not** a
/// `minusExpr`, so it cannot be a `<-` assignment LHS: FCS rejects the `<-`
/// ("Unexpected symbol '<-' in binding"). The `fixed` operand
/// (`parse_expr`) builds the `:>` cast and so declines the trailing `<-`,
/// returning it pending; the outer Pratt frame must *not* fold it into an
/// `ASSIGN_EXPR` over the `FIXED_EXPR` (the `lhs_is_fixed` gate). Our-side
/// recovery check: error, lossless, and no `ASSIGN_EXPR` wrapping the `fixed`.
#[test]
fn fixed_cast_then_arrow_does_not_assign_to_fixed() {
    let src = "let x = fixed a :> T <- v\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error: `fixed …` is not a `<-` assignment LHS in `{src}`",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must be lossless even on the recovery path",
    );
    // The `<-` must be left for recovery, not bound over the `FIXED_EXPR`. The
    // *only* `ASSIGN_EXPR` allowed would be one *inside* the operand (there is
    // none here — `a :> T` has no `<-`), so a top-level `ASSIGN_EXPR` whose first
    // child is the `FIXED_EXPR` is the bug this guards against.
    let bad = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::ASSIGN_EXPR
            && n.children().any(|c| c.kind() == SyntaxKind::FIXED_EXPR)
    });
    assert!(
        !bad,
        "the `<-` must not bind the completed FIXED_EXPR as an assignment target",
    );
}
