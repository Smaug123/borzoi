//! Differential test (`parser::parse` vs FCS): `if`/`then`/`else` and `elif`
//! conditional expressions, plus `while … do …` loops (phase 10.4d). Split out
//! of the former monolithic `parser_diff.rs`.

use crate::common::{assert_asts_match, assert_asts_match_allow_errors};

/// Phase 5.1 — `if true then 1 else 2`: the smallest valid `if`/`then`/
/// `else` expression. Exercises the basic dispatch in `parse_pratt_expr`,
/// the `Virtual::Then` / `Virtual::Else` keyword emission, and the
/// `BlockBegin` / `BlockEnd` scaffolding handling. FCS projects to
/// `SynExpr.IfThenElse` with `elseExpr = Some _`.
#[test]
fn diff_ast_if_then_else_simple() {
    assert_asts_match("if true then 1 else 2\n");
}

/// Phase 5.1 — `if true then x else y`: identifier branches.
/// Confirms the branches go through the same expression parser as
/// the condition (idents project to `SynExpr.Ident`).
#[test]
fn diff_ast_if_then_else_ident_branches() {
    assert_asts_match("if true then x else y\n");
}

/// Phase 5.1 — `if true then 1 else 2 + 3`: the else-branch greedily
/// absorbs the trailing `+ 3` via its inner `parse_pratt_expr(0)`.
/// Mirrors FCS's `ELSE declExpr` grammar — `expr_if` precedence sits
/// below `+`'s level so the infix shifts into the else-branch.
#[test]
fn diff_ast_if_then_else_infix_in_else() {
    assert_asts_match("if true then 1 else 2 + 3\n");
}

/// Phase 5.1 — `if a then b + c else d`: infix inside the then-branch.
/// `parse_pratt_expr(0)` for the then-branch stops at `Virtual::Else`
/// because the keyword isn't an infix continuation; `+` is captured
/// inside the branch.
#[test]
fn diff_ast_if_then_else_infix_in_then() {
    assert_asts_match("if a then b + c else d\n");
}

/// Phase 5.1 — `(if true then 1 else 2) + 3`: parenthesised if-then-else
/// followed by an infix continuation. The LexFilter emits a
/// `Virtual::BlockEnd` closing the else-body's SeqBlock right before
/// the swallowed `)`, so `parse_if_then_else` must drain that virtual
/// before returning; otherwise the outer Pratt loop sees the BlockEnd
/// (not the `+`) and the `+ 3` continuation is lost.
#[test]
fn diff_ast_if_then_else_paren_then_infix() {
    assert_asts_match("(if true then 1 else 2) + 3\n");
}

/// Phase 5.1 — `if true then 1 else 2, 3`: tuple-valued else branch.
/// FCS gives `expr_if` lower precedence than `COMMA` (`pars.fsy:323`),
/// so the comma shifts into the else-branch — the AST is
/// `IfThenElse(true, 1, Tuple(2, 3))`, NOT
/// `Tuple(IfThenElse(true, 1, 2), 3)`. The else-branch must therefore
/// parse at the tuple level (`parse_expr`), not just Pratt.
#[test]
fn diff_ast_if_then_else_tuple_in_else() {
    assert_asts_match("if true then 1 else 2, 3\n");
}

/// Phase 5.1 — `let x = if … else …\nlet y = 3`: an if-expression as
/// the RHS of a `let` followed by a sibling decl. LexFilter emits two
/// `Virtual::BlockEnd`s after the else-body: one closing the
/// if-then-else's SeqBlock, and one closing the let RHS's offside
/// block, before the `Virtual::DeclEnd` for the let. `parse_if_then_else`
/// must consume only its own BlockEnd (and any sibling `BlockSep`s
/// belonging to that scope), leaving the enclosing let's BlockEnd
/// in place — otherwise the outer let-parser misses its RHS terminator
/// and starts absorbing `let y = 3` into the first binding.
#[test]
fn diff_ast_if_then_else_let_rhs_then_sibling_decl() {
    assert_asts_match("let x = if true then 1 else 2\nlet y = 3\n");
}

/// Phase 5.2 — `if true then 1`: the no-else form. FCS projects to
/// `SynExpr.IfThenElse` with `elseExpr = None`. Our normaliser tracks
/// the else-branch as `Option<Box<NormalisedExpr>>` and surfaces the
/// missing else as `None` on both sides — the diff oracle just
/// compares structures.
#[test]
fn diff_ast_if_then_no_else_simple() {
    assert_asts_match("if true then 1\n");
}

/// Phase 5.2 — `(if true then 1) + 2`: a no-else `if` inside parens
/// followed by an infix continuation. The then-body's
/// `Virtual::BlockEnd` lands at the swallowed `)` byte position; the
/// no-else close must not steal that `)` from `parse_paren_expr`.
/// FCS projects to `App(+, Paren(IfThenElse(_, _, None)), 2)`.
#[test]
fn diff_ast_if_then_no_else_paren_then_infix() {
    assert_asts_match("(if true then 1) + 2\n");
}

/// Phase 5.2 — `let x = if true then 1\nlet y = 2`: a no-else `if`
/// on a let RHS followed by a sibling let. The then-body's
/// `Virtual::BlockEnd` closes the if; the let RHS's BlockEnd closes
/// the binding; the impl-file loop then picks up `let y`. Mirrors
/// the Phase 5.1 sibling test but with no else.
#[test]
fn diff_ast_if_then_no_else_let_rhs_then_sibling_decl() {
    assert_asts_match("let x = if true then 1\nlet y = 2\n");
}

/// Phase 5.2 — `if c1 then if c2 then 1`: nested no-else forms.
/// Each inner BlockEnd terminates its own scope; the outer if must
/// not greedily look past its own BlockEnd in search of an else.
/// FCS projects to `IfThenElse(c1, IfThenElse(c2, 1, None), None)`.
#[test]
fn diff_ast_if_then_no_else_nested() {
    assert_asts_match("if c1 then if c2 then 1\n");
}

/// Phase 5.3 — `if a then 1 elif b then 2 else 3`: a single `elif`
/// arm with a trailing else. FCS encodes the chain by nesting an
/// inner `IfThenElse` in the outer's `elseExpr`, so the projection
/// is `IfThenElse(a, 1, Some(IfThenElse(b, 2, Some(3))))`. Our
/// parser shapes a nested `IF_THEN_ELSE_EXPR` in the outer's else
/// slot, which the normaliser projects the same way.
#[test]
fn diff_ast_elif_with_trailing_else() {
    assert_asts_match("if a then 1 elif b then 2 else 3\n");
}

/// Phase 5.3 — `if a then 1 elif b then 2 elif c then 3 else 4`:
/// two chained `elif` arms. Drives the recursive
/// `parse_if_then_else_tail` path twice; the projection is
/// `IfThenElse(a, 1, Some(IfThenElse(b, 2, Some(IfThenElse(c, 3,
/// Some(4))))))`.
#[test]
fn diff_ast_two_elif_arms() {
    assert_asts_match("if a then 1 elif b then 2 elif c then 3 else 4\n");
}

/// Phase 5.3 — `if a then 1 elif b then 2`: elif chain with no
/// trailing else. The inner (elif) `IfThenElse` has `elseExpr =
/// None`. Projection:
/// `IfThenElse(a, 1, Some(IfThenElse(b, 2, None)))`.
#[test]
fn diff_ast_elif_without_trailing_else() {
    assert_asts_match("if a then 1 elif b then 2\n");
}

/// Phase 5.3 — `(if a then 1 elif b then 2 else 3) + 4`: elif chain
/// inside parens followed by an infix continuation. Confirms the
/// nested elif's BlockEnd cascade still terminates at the closing
/// `)` so the outer `+` continuation is preserved. FCS projects to
/// `App(+, Paren(IfThenElse(a, 1, Some(IfThenElse(b, 2, Some(3))))), 4)`.
#[test]
fn diff_ast_elif_inside_parens_then_infix() {
    assert_asts_match("(if a then 1 elif b then 2 else 3) + 4\n");
}

/// Phase 5.3 — `let x = if a then 1 elif b then 2 else 3`: elif
/// chain on a let RHS. Exercises the BlockEnd accounting through
/// the nested elif and back out to the enclosing let.
#[test]
fn diff_ast_elif_at_let_rhs() {
    assert_asts_match("let x = if a then 1 elif b then 2 else 3\n");
}

/// Phase 5.3 — `let x = if a then 1 elif b then 2 else 3\nlet y = 4`:
/// elif chain at let RHS followed by a sibling decl. Pins that the
/// elif's BlockEnd cascade doesn't swallow the let's closing
/// BlockEnd; the impl-file loop must cleanly pick up `let y` as a
/// sibling binding.
#[test]
fn diff_ast_elif_at_let_rhs_then_sibling_decl() {
    assert_asts_match("let x = if a then 1 elif b then 2 else 3\nlet y = 4\n");
}

/// Stage 2 — explicit `;` sequential in an `if` then-branch (no else):
/// `if c then a; b` ⇒ `IfThenElse(c, Sequential(a, b), None)`.
#[test]
fn diff_ast_if_then_semi_seq_body() {
    assert_asts_match("if c then a; b\n");
}

/// Stage 2 — explicit `;` in a then-branch with an `else`: the gatherer must
/// stop at `else`, so `if c then a; b else d` ⇒
/// `IfThenElse(c, Sequential(a, b), d)`.
#[test]
fn diff_ast_if_then_else_semi_seq_body() {
    assert_asts_match("if c then a; b else d\n");
}

// ============================================================================
// Phase 10.4d — `while … do …` (`SynExpr.While`)
// ============================================================================
//
// `while cond do body` is `SynExpr.While(whileDebugPoint, whileExpr, doExpr,
// range)` (`SyntaxTree.fsi:656`). `Token::While` is a plain raw token
// (LexFilter pushes `CtxtWhile`/`CtxtDo` but does not relabel it); the `do`
// keyword surfaces as `Virtual::Do` (`ODO`) backed by the raw `Token::Do` at
// the same span, and the body is a SeqBlock (`BlockBegin … BlockEnd DeclEnd`) —
// the same scaffolding `do!`/`if` bodies use, so the cond + `do` + body parse
// reuses `parse_if_body`. Both sides project to `NormalisedExpr::While { cond,
// body }` (debug-point + range elided). This is the substrate `while!`
// (10.4e) rides on.

/// The smallest loop: `while c do ()`. Cond is an ident, body the unit literal.
#[test]
fn diff_ast_while_basic() {
    assert_asts_match("while c do ()\n");
}

/// `while` on a `let`-binding RHS — pins that the loop body's `BlockEnd`/
/// `DeclEnd` close survives back out to the enclosing binding (the `match`/`if`
/// let-RHS guard, applied to `while`).
#[test]
fn diff_ast_while_let_rhs() {
    assert_asts_match("let f () = while c do g ()\n");
}

/// Multi-statement offside body ⇒ the `doExpr` is a `SynExpr.Sequential`
/// (`while c do⏎  a⏎  b`). Exercises the `parse_if_body` `Virtual::BlockSep`
/// loop wrapping the body in `SEQUENTIAL_EXPR`.
#[test]
fn diff_ast_while_multi_statement_body() {
    assert_asts_match("while c do\n  a\n  b\n");
}

/// Nested `while` as the body of another `while` — the inner loop's
/// `BlockEnd`/`DeclEnd` cascade must not be mistaken for the outer's.
#[test]
fn diff_ast_while_nested() {
    assert_asts_match("while a do while b do ()\n");
}

/// `while` inside a computation expression (`seq { while c do () }`) ⇒
/// `ComputationExpr(App(seq, While(…)))`. Confirms the loop composes as a CE
/// body and the swallowed `}` still reaches `bump_swallowed_closer`.
#[test]
fn diff_ast_while_in_ce() {
    assert_asts_match("seq { while c do () }\n");
}

/// Explicit verbose-syntax `done` terminator: `while c do f done`. LexFilter
/// relabels the raw `done` to the body's closing `Virtual::DeclEnd` at the
/// `done` span (keeping the raw token); the loop must claim it as `DONE_TOK`,
/// not leave it as an unsupported leftover. FCS parses this cleanly (the `done`
/// keyword is not stored in `SynExpr.While`, so both sides project the same
/// `While { cond: c, body: f }`).
#[test]
fn diff_ast_while_done_terminator() {
    assert_asts_match("while c do f done\n");
}

/// `done` terminating a unit-literal body (`while c do () done`) — the common
/// shape, with the body delimiters and the `done` adjacent.
#[test]
fn diff_ast_while_done_unit_body() {
    assert_asts_match("while c do () done\n");
}

// ---------------------------------------------------------------------------
// `for … in … do …` — `SynExpr.ForEach`
// ---------------------------------------------------------------------------
//
// `for pat in enumExpr do body` is `SynExpr.ForEach(forDebugPoint,
// inDebugPoint, seqExprOnly, isFromSource, pat, enumExpr, bodyExpr, range)`
// (`SyntaxTree.fsi:671`). The normaliser keeps `pat`, `enumExpr`, `body`; the
// debug points, `seqExprOnly`, `isFromSource`, and range are elided. The `in`
// is a raw `Token::In` (not the `let … in` relabel), `do` is `Virtual::Do`,
// and the body reuses `while`'s `BlockBegin … BlockEnd DeclEnd` scaffold.

/// The smallest enumerator loop: `for x in xs do ()`. Pat is a `Named`, the
/// collection an ident, the body the unit literal.
#[test]
fn diff_ast_for_each_basic() {
    assert_asts_match("for x in xs do ()\n");
}

/// The reported failure: a dotted-path collection (`xml.UnprocessedLines`)
/// projects to a `LongIdent` enum expression.
#[test]
fn diff_ast_for_each_dotted_collection() {
    assert_asts_match("for line in xml.UnprocessedLines do f line\n");
}

/// Multi-statement offside body (`for x in xs do⏎  a⏎  b`) — exercises the
/// shared `parse_if_body` `Virtual::BlockSep` → `SEQUENTIAL_EXPR` path.
#[test]
fn diff_ast_for_each_multi_statement_body() {
    assert_asts_match("for x in xs do\n  a\n  b\n");
}

/// A tuple pattern binder (`for (i, x) in indexed do …`) — the binder is a
/// full `parenPattern`.
#[test]
fn diff_ast_for_each_tuple_pattern() {
    assert_asts_match("for (i, x) in indexed do f i x\n");
}

/// An *unparenthesized* tuple binder (`for i, x in indexed do …`) — FCS's
/// `forLoopBinder` is a full `parenPattern`, which includes the bare tuple.
#[test]
fn diff_ast_for_each_bare_tuple_pattern() {
    assert_asts_match("for i, x in indexed do f i x\n");
}

/// An *unparenthesized* type-annotated binder (`for x : int in xs do …`).
/// FCS's `forLoopBinder` is a full `parenPattern`, so the `:` belongs to the
/// binder (projecting to a `Typed` pattern) — this only parses cleanly with
/// `PatCtx::Paren`, not the match-clause-head `Clause` context.
#[test]
fn diff_ast_for_each_typed_pattern() {
    assert_asts_match("for x : int in xs do f x\n");
}

/// A parenthesized type-annotated binder (`for (x : int) in xs do …`) — the
/// `:` is inside the parens, projecting to `Paren(Typed …)`.
#[test]
fn diff_ast_for_each_paren_typed_pattern() {
    assert_asts_match("for (x : int) in xs do f x\n");
}

/// An application collection (`for x in items () do …`) — the enum expr is an
/// `APP_EXPR`, exercising a non-atomic collection. (A range-expression
/// collection `for x in 0 .. 9 do …` is blocked on `SynExpr.IndexRange`, which
/// is not yet parsed — a separate slice.)
#[test]
fn diff_ast_for_each_app_collection() {
    assert_asts_match("for x in items () do f x\n");
}

/// `for` on a `let`-binding RHS — pins the loop body's `BlockEnd`/`DeclEnd`
/// drain against the enclosing let-RHS guard (mirrors `diff_ast_while_let_rhs`).
#[test]
fn diff_ast_for_each_let_rhs() {
    assert_asts_match("let f () = for x in xs do g x\n");
}

/// `for` inside a computation expression (`seq { for x in xs do () }`).
#[test]
fn diff_ast_for_each_in_ce() {
    assert_asts_match("seq { for x in xs do () }\n");
}

/// Nested `for` as the body of another `for` — the inner loop's `done`/`DeclEnd`
/// span must match the inner loop (mirrors `diff_ast_while_nested`).
#[test]
fn diff_ast_for_each_nested() {
    assert_asts_match("for x in xs do for y in ys do ()\n");
}

/// Explicit verbose-syntax `done` terminator: `for x in xs do f x done`.
#[test]
fn diff_ast_for_each_done_terminator() {
    assert_asts_match("for x in xs do f x done\n");
}

// ---------------------------------------------------------------------------
// `for pat in e -> body` — the comprehension arrow form (`SynExpr.ForEach`
// with a `YieldOrReturn` body)
// ---------------------------------------------------------------------------
//
// `for pat in enumExpr -> body` (`pars.fsy:4412`) is the same `SynExpr.ForEach`
// node as the `do` form but with `seqExprOnly = true` (elided) and the body
// desugared to `SynExpr.YieldOrReturn((true, false), body)` (`arrowThenExprR`,
// `pars.fsy:5608`) — an implicit `yield`. The body is built as a
// `YIELD_OR_RETURN_EXPR` carrying the `->` instead of a `yield` keyword.

/// The canonical comprehension, inside a `seq { … }` computation expression.
#[test]
fn diff_ast_for_each_arrow_in_seq() {
    assert_asts_match("let z = seq { for x in xs -> x }\n");
}

/// The arrow form at the top level (`for x in xs -> x`) — FCS accepts it as a
/// bare `declExpr`, projecting the same yield-wrapped `ForEach`.
#[test]
fn diff_ast_for_each_arrow_top_level() {
    assert_asts_match("for x in xs -> x\n");
}

/// An application body after the arrow (`for x in xs -> g x`) — the yielded
/// expression is an `APP_EXPR`.
#[test]
fn diff_ast_for_each_arrow_app_body() {
    assert_asts_match("for x in xs -> g x\n");
}

/// A tuple binder with the arrow form (`for (i, x) in indexed -> i`).
#[test]
fn diff_ast_for_each_arrow_tuple_pattern() {
    assert_asts_match("for (i, x) in indexed -> i\n");
}

/// The arrow body split across a line (`for x in xs ->⏎  g x`) — still a single
/// yielded expression; pins the one-sided SeqBlock `RightBlockEnd` drain.
#[test]
fn diff_ast_for_each_arrow_offside_body() {
    assert_asts_match("for x in xs ->\n  g x\n");
}

/// The `->` on a continuation line inside a `seq { … }` — FCS emits the
/// `opt_OBLOCKSEP` `Virtual::BlockSep` before the arrow, which the dispatch
/// must skip (else the loop is misparsed as a missing `do`).
#[test]
fn diff_ast_for_each_arrow_blocksep_before_arrow() {
    assert_asts_match("let z = seq { for x in xs\n              -> g x }\n");
}

// ---------------------------------------------------------------------------
// `for i = a to/downto b do …` — `SynExpr.For`
// ---------------------------------------------------------------------------
//
// `for ident = identBody to/downto toBody do doBody` is `SynExpr.For(
// forDebugPoint, toDebugPoint, ident, equalsRange, identBody, direction,
// toBody, doBody, range)` (`SyntaxTree.fsi:659`). The normaliser keeps the
// loop variable (`idText`), both bounds, the body, and `direction` (`to` =
// ascending, `downto` = descending); the debug points, `equalsRange`, and
// range are elided. FCS's `forLoopRange` is `parenPattern EQUALS …` reduced to
// an ident via `idOfPat`; for valid input the binder is a bare ident, selected
// via the `Ident =` lookahead.

/// The smallest ascending range loop: `for i = 1 to 10 do f i`.
#[test]
fn diff_ast_for_range_to() {
    assert_asts_match("for i = 1 to 10 do f i\n");
}

/// A descending range loop: `for i = 10 downto 1 do f i` — `direction = false`.
#[test]
fn diff_ast_for_range_downto() {
    assert_asts_match("for i = 10 downto 1 do f i\n");
}

/// A wildcard loop variable (`for _ = 1 to 10 do g ()`) — FCS's `idOfPat`
/// accepts `SynPat.Wild` (the F# 4.7+ `WildCardInForLoop` feature), projecting
/// the loop variable to a synthetic ident with `idText = "_"`. This selects the
/// range form via the `_ =` lookahead, not the enumerator path.
#[test]
fn diff_ast_for_range_wildcard_binder() {
    assert_asts_match("for _ = 1 to 10 do g ()\n");
}

/// Expression bounds (`for i = lo to hi - 1 do …`) — both bounds are full
/// expressions, not just literals; the start bound stops at the raw `to` and
/// the end bound at `Virtual::Do`.
#[test]
fn diff_ast_for_range_expression_bounds() {
    assert_asts_match("for i = lo to hi - 1 do f i\n");
}

/// Multi-statement offside body (`for i = 1 to 10 do⏎  a⏎  b`).
#[test]
fn diff_ast_for_range_multi_statement_body() {
    assert_asts_match("for i = 1 to 10 do\n  a\n  b\n");
}

/// Explicit verbose-syntax `done` terminator: `for i = 1 to 10 do f i done`.
#[test]
fn diff_ast_for_range_done_terminator() {
    assert_asts_match("for i = 1 to 10 do f i done\n");
}

/// `for` range on a `let`-binding RHS — pins the loop body's `BlockEnd`/
/// `DeclEnd` drain against the enclosing let-RHS guard.
#[test]
fn diff_ast_for_range_let_rhs() {
    assert_asts_match("let f () = for i = 1 to 10 do g i\n");
}

/// A range loop nested as the body of an enumerator loop, mixing the two forms.
#[test]
fn diff_ast_for_range_nested_in_for_each() {
    assert_asts_match("for xs in xss do for i = 1 to 10 do f i\n");
}

// ============================================================================
// Top-level `do` — `SynExpr.Do`
//
// In #light syntax a `do e` statement flows through FCS's `declExpr`
// (`hardwhiteDoBinding`, `pars.fsy:4211`) to `SynExpr.Do(e, range)`, and a
// module-level `declExpr` is wrapped in `SynModuleDecl.Expr`. So `do ()` is
// `Expr(Do(Const Unit))` — a `DO_EXPR` inside the ordinary `EXPR_DECL` path,
// reusing the `while`/`for` `do`-body SeqBlock scaffolding above.
// ============================================================================

/// The headline case: a bare top-level `do ()`. Unit-literal body.
#[test]
fn diff_ast_top_level_do_unit() {
    assert_asts_match("do ()\n");
}

/// A non-unit body (`do printfn "x"`) — the `do`-bound expression is an
/// application, so `Do(App(Ident "printfn", Const "x"))`.
#[test]
fn diff_ast_top_level_do_application_body() {
    assert_asts_match("do printfn \"x\"\n");
}

/// Multi-statement offside body (`do⏎  a⏎  b`) ⇒ `Do(Sequential[a; b])` —
/// exercises the SeqBlock gatherer (`parse_seq_block_body`) under the `do`.
#[test]
fn diff_ast_top_level_do_multi_statement_body() {
    assert_asts_match("do\n  a\n  b\n");
}

/// Two consecutive top-level `do`s — each is its own `Expr(Do …)` decl,
/// separated by the offside `Virtual::BlockSep`.
#[test]
fn diff_ast_top_level_do_two_decls() {
    assert_asts_match("do f ()\ndo g ()\n");
}

/// A top-level `do` followed by a sibling decl — pins that the `do`-body
/// SeqBlock close doesn't swallow the following statement.
#[test]
fn diff_ast_top_level_do_then_let() {
    assert_asts_match("do f ()\nlet x = 1\n");
}

/// `do` as a statement inside a `let`-binding's offside body (nested
/// expression position): `let f () =⏎  do g ()⏎  h ()`. The `do` is a
/// `SynExpr.Do` element of the body `Sequential`.
#[test]
fn diff_ast_do_in_let_body_sequence() {
    assert_asts_match("let f () =\n  do g ()\n  h ()\n");
}

/// `do` as the body of a parenthesised expression (`let x = (do f)`). FCS
/// accepts a `(`-leading `do`, so the paren-after lookahead must admit it.
#[test]
fn diff_ast_paren_do() {
    assert_asts_match("let x = (do f)\n");
}

/// `do` as a parenthesised application argument (`f (do g)`) — the same
/// paren-after gate, reached through an app-arg paren.
#[test]
fn diff_ast_paren_do_as_app_arg() {
    assert_asts_match("f (do g)\n");
}

/// `do` heading a parenthesised *sequence* (`(do f; 3)`) ⇒ `Paren(Sequential[
/// Do f; 3])` — the `do` is the first statement of the paren seq-block.
#[test]
fn diff_ast_paren_do_then_seq() {
    assert_asts_match("let y = (do f; 3)\n");
}

/// `do f, 3` ⇒ `Do(Tuple(f, 3))` — the `do` body is FCS's low-precedence
/// `typedSequentialExprBlock`, so it greedily takes the whole tuple (it is
/// *not* `Tuple(Do f, 3)`). Confirms the `do`-body `parse_expr` is
/// tuple-inclusive, matching the grammar's `%prec expr_let`.
#[test]
fn diff_ast_top_level_do_tuple_body() {
    assert_asts_match("do f, 3\n");
}

/// Regression: making `do` an expression-starter must NOT pull the `do` of a
/// `while`/`for` loop into the condition. These mirror the basic loop oracles
/// above and must keep projecting to `SynExpr.While` / `SynExpr.ForEach`.
#[test]
fn diff_ast_while_do_unaffected_by_do_expr() {
    assert_asts_match("while c do ()\n");
}

#[test]
fn diff_ast_for_each_do_unaffected_by_do_expr() {
    assert_asts_match("for x in xs do ()\n");
}

// ---- Phase 11 error recovery: incomplete `if`/`then`/`else` ----------------
//
// A recovery hole in any branch — `if c then`, `if c then a else`, or the
// middle hole `if c then else b` — is filled by FCS with
// `SynExpr.ArbitraryAfterError`; our parser leaves a zero-width `ERROR`, which
// the normaliser projects to the shared `NormalisedExpr::Error` marker. The
// branch accessors are *keyword-relative* (resolved around `THEN_TOK`/
// `ELSE_TOK`), so each hole is attributed to the correct slot — `if c then
// else b` recovers to `IfThenElse(c, Error, b)`, not the wrong
// `IfThenElse(c, b, None)` a positional read would give. The trailing decl
// survives in every case.

/// `if true then` at end of file — FCS: `IfThenElse(Const true,
/// ArbitraryAfterError, None)`; ours matches after projecting the hole.
#[test]
fn diff_ast_if_then_recover_missing_then_eof() {
    assert_asts_match_allow_errors("if true then\n");
}

/// `if a then` as a binding RHS, with the following `let` surviving as its own
/// decl — one incomplete `if` does not collapse the rest of the file.
#[test]
fn diff_ast_if_then_recover_missing_then_then_decl() {
    assert_asts_match_allow_errors("let x = if a then\nlet y = 2\n");
}

/// Missing else-*expression* after the `else` keyword — `if a then b else`.
/// `has_else` is true (the keyword is present), so the else slot recovers to
/// `Some(Error)`, matching FCS's `IfThenElse(a, b, Some ArbitraryAfterError)` —
/// distinct from the no-`else` form, which keeps `elseExpr = None`.
#[test]
fn diff_ast_if_then_recover_missing_else_expr() {
    assert_asts_match_allow_errors("let x = if a then b else\nlet y = 2\n");
}

/// Missing *middle* (then) branch with an `else` present — `if a then else c`.
/// Keyword-relative resolution attributes `c` to the else slot (after `else`)
/// and the empty then slot to `Error`: `IfThenElse(a, Error, c)`, matching FCS.
#[test]
fn diff_ast_if_then_recover_missing_then_with_else() {
    assert_asts_match_allow_errors("let x = if a then else c\nlet y = 2\n");
}

/// Missing then-branch before a bare `elif` — `if a then elif b then c`. The
/// `elif` is a nested `IF_THEN_ELSE` with no `ELSE_TOK` at the outer level, so
/// it must be recognised as the *else* slot (not the then-branch): FCS gives
/// `IfThenElse(a, Error, Some(IfThenElse(b, c, None)))`, which ours matches.
#[test]
fn diff_ast_if_then_recover_missing_then_before_elif() {
    assert_asts_match_allow_errors("let x = if a then elif b then c\nlet y = 2\n");
}
