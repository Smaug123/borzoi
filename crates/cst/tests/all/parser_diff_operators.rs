//! Differential test (`parser::parse` vs FCS): tuples, parenthesisation,
//! application, and infix/prefix operators. Split out of the former
//! monolithic `parser_diff.rs`.

use crate::common::{
    assert_asts_match, assert_asts_match_allow_errors, assert_asts_match_fcs_rejects_ours_accepts,
};

/// `( 1 )` — paren expression around an int. FCS produces
/// `SynExpr.Paren(SynExpr.Const(SynConst.Int32 1), …)`; we emit
/// `PAREN_EXPR > [LPAREN_TOK, CONST_EXPR > INT32_LIT, RPAREN_TOK]` and
/// both sides project to `NormalisedExpr::Paren(Const(Int32 1))`.
#[test]
fn diff_ast_paren_around_int() {
    assert_asts_match("( 1 )\n");
}

/// `(x)` — paren around an ident. Mirrors `diff_ast_paren_around_int` for
/// the `SynExpr.Ident` interior.
#[test]
fn diff_ast_paren_around_ident() {
    assert_asts_match("(x)\n");
}

/// `((1))` — nested paren expressions. FCS produces two nested
/// `SynExpr.Paren` wrappers around the inner `Int32 1`; the projector
/// must recurse to match.
#[test]
fn diff_ast_nested_paren() {
    assert_asts_match("((1))\n");
}

/// `(Foo.Bar)` — paren around a dotted path. Confirms paren-expr wraps
/// `SynExpr.LongIdent` too.
#[test]
fn diff_ast_paren_around_long_ident() {
    assert_asts_match("(Foo.Bar)\n");
}

/// `1, 2` — a two-element tuple at the top level. FCS produces
/// `SynExpr.Tuple(false, [Int32 1; Int32 2], [commaRange], _)`; we emit
/// `TUPLE_EXPR > [CONST_EXPR > INT32_LIT, COMMA_TOK, CONST_EXPR >
/// INT32_LIT]` and both sides project to `NormalisedExpr::Tuple { is_struct
/// = false, elements = [Int32 1, Int32 2] }`.
#[test]
fn diff_ast_two_tuple() {
    assert_asts_match("1, 2\n");
}

/// `1, 2, 3` — three-element tuple. Pins that `parse_expr`'s comma-loop
/// keeps wrapping under the same TUPLE_EXPR (rather than nesting), matching
/// FCS's flat `SynExpr.Tuple` element list.
#[test]
fn diff_ast_three_tuple() {
    assert_asts_match("1, 2, 3\n");
}

/// `(1, 2)` — tuple inside a paren. FCS emits `SynExpr.Paren(SynExpr.Tuple
/// (false, [Int32 1; Int32 2], …), …)`; we produce the same nesting via
/// `PAREN_EXPR > [LPAREN_TOK, TUPLE_EXPR > [...], RPAREN_TOK]`. Pins that
/// the comma-loop runs inside the recursive `parse_expr` call from
/// `parse_paren_expr`.
#[test]
fn diff_ast_tuple_in_paren() {
    assert_asts_match("(1, 2)\n");
}

/// `x, y` — tuple of two single-segment idents. Confirms tuple wrapping
/// works over `SynExpr.Ident` element shapes, not just constants.
#[test]
fn diff_ast_tuple_of_idents() {
    assert_asts_match("x, y\n");
}

/// `(1), 2` — paren-wrapped first element of an outer tuple. FCS produces
/// `SynExpr.Tuple([SynExpr.Paren(Int32 1, …); Int32 2], …)`. Because
/// LexFilter swallows `)`, the inner `parse_expr` peeks the filtered
/// stream and sees the *outer* comma directly; the tuple loop must
/// gate on the raw stream to avoid eating it.
#[test]
fn diff_ast_paren_then_comma_outer_tuple() {
    assert_asts_match("(1), 2\n");
}

/// `(1, (2), 3)` — middle element of a 3-tuple is parenthesised. Same
/// regression class as `diff_ast_paren_then_comma_outer_tuple`: the
/// nested `parse_expr` must not consume the outer comma after its own
/// swallowed `)`.
#[test]
fn diff_ast_nested_paren_in_middle_of_tuple() {
    assert_asts_match("(1, (2), 3)\n");
}

/// Multi-line tuple inside parens. LexFilter inserts a `Virtual::BlockSep`
/// between the aligned `1,` and `2` lines; the tuple loop must skip it
/// rather than treat it as a missing element. FCS produces the same
/// `SynExpr.Paren(SynExpr.Tuple([1; 2], …), …)` shape as the single-line
/// form. Regression pin for codex review of phase 3.2.
#[test]
fn diff_ast_multiline_tuple_in_paren() {
    assert_asts_match("(\n    1,\n    2\n)\n");
}

/// `f x` — whitespace-separated function application. FCS produces
/// `SynExpr.App(NonAtomic, false, Ident "f", Ident "x", _)`; we produce
/// `APP_EXPR > [IDENT_EXPR, IDENT_EXPR]`, and both normalise to
/// `App { is_atomic = false, is_infix = false, func = Ident "f", arg =
/// Ident "x" }`. The Atomic-flag form `f(x)` is deferred (lexfilter
/// doesn't emit `HIGH_PRECEDENCE_PAREN_APP` yet).
#[test]
fn diff_ast_app_two_idents() {
    assert_asts_match("f x\n");
}

/// `f x y` — three-segment application. F# applications are
/// left-associative: FCS nests as `App(App(f, x), y)`. The same
/// `Checkpoint` reused each iteration of `parse_app_expr` produces the
/// matching nesting.
#[test]
fn diff_ast_app_three_idents() {
    assert_asts_match("f x y\n");
}

/// `f 1` — function applied to a constant. Confirms application doesn't
/// care which expression-starter shows up in the argument position
/// (`IDENT_EXPR` head, `CONST_EXPR` arg here).
#[test]
fn diff_ast_app_ident_to_int() {
    assert_asts_match("f 1\n");
}

/// `f (g x)` — application whose argument is a paren-wrapped
/// sub-application. The outer `parse_app_expr` parses `f` as the head,
/// then the `(` argument routes through `parse_paren_expr` → recursive
/// `parse_expr` → inner `App(g, x)`. FCS produces
/// `App(NonAtomic, false, Ident "f", Paren(App(NonAtomic, false, g, x)))`.
#[test]
fn diff_ast_app_with_paren_arg() {
    assert_asts_match("f (g x)\n");
}

/// `f x, g y` — application binds tighter than the tuple comma. FCS
/// produces `Tuple([App(f, x); App(g, y)], …)`. The crucial precedence
/// interaction: `parse_expr` calls `parse_app_expr` for each tuple element,
/// so each element exhausts its application greedily before the comma loop
/// continues.
#[test]
fn diff_ast_app_in_tuple_elements() {
    assert_asts_match("f x, g y\n");
}

/// `f x\ng y\n` — two top-level app decls separated by a
/// `Virtual::BlockSep`. `parse_app_expr` stops at BlockSep because it's
/// not in `peek_is_expr_start`; the outer impl_file loop then bumps the
/// virtual and starts a fresh decl. FCS produces two decls (one App per
/// line) with no extra wrapper.
#[test]
fn diff_ast_two_apps_across_newline() {
    assert_asts_match("f x\ng y\n");
}

/// `Foo.Bar x` — long-ident head applied to an ident. Confirms App
/// works with a `SynExpr.LongIdent` in the function position.
#[test]
fn diff_ast_app_long_ident_head() {
    assert_asts_match("Foo.Bar x\n");
}

/// Indented continuation: `42\n  43` is a single `App(42, 43)` because
/// LexFilter does NOT emit a `Virtual::BlockSep` when the next token is
/// indented past the first. The offside layout makes the second line a
/// continuation of the expression, not a new decl. FCS confirms the App
/// shape.
#[test]
fn diff_ast_app_indented_continuation() {
    assert_asts_match("42\n  43\n");
}

/// `a + b` — the minimal infix differential. FCS lowers via `mkSynInfix`
/// to `App(NonAtomic, false, App(NonAtomic, true, op, lhs), rhs)`, with
/// the op as a single-segment `SynLongIdent` whose `idText = "op_Addition"`
/// and trivia carries `IdentTrivia.OriginalNotation "+"`. The FCS-side
/// normaliser unwraps the trivia so it reads back as `LongIdent(["+"])`,
/// matching our green tree which stores `+` directly in the `IDENT_TOK`.
#[test]
fn diff_ast_infix_plus_two_idents() {
    assert_asts_match("a + b\n");
}

/// `a + b * c` — `*` (INFIX_STAR_DIV_MOD_OP) binds tighter than `+`
/// (PLUS_MINUS_OP) per pars.fsy lines 364–365. FCS produces
/// `App(+, a, App(*, b, c))`; our Pratt climber agrees because
/// `parse_pratt_expr(rbp = 61)` for the `+`'s RHS keeps consuming
/// `*` (lbp = 70 ≥ 61).
#[test]
fn diff_ast_infix_precedence_plus_times() {
    assert_asts_match("a + b * c\n");
}

/// `a + b + c` — left-associative per pars.fsy line 364
/// (`%left PLUS_MINUS_OP`). FCS produces `App(+, App(+, a, b), c)`.
/// Confirms our `rbp = lbp - 1` encoding for left-associative ops.
#[test]
fn diff_ast_infix_plus_left_associative() {
    assert_asts_match("a + b + c\n");
}

/// `a, b + c` — tuple commas sit below all infix in pars.fsy; FCS
/// produces `Tuple([a; App(+, b, c)])`. Pins the precedence interaction
/// between `parse_expr`'s tuple loop and `parse_pratt_expr`.
#[test]
fn diff_ast_infix_inside_tuple_element() {
    assert_asts_match("a, b + c\n");
}

/// `f a + b` — application (`expr_app` in pars.fsy) sits above all
/// infix bands. FCS produces `App(+, App(f, a), b)`; the Pratt climber
/// agrees because `parse_app_expr` greedily exhausts the
/// `f a` application chain before the operator scan kicks in.
#[test]
fn diff_ast_app_tighter_than_infix() {
    assert_asts_match("f a + b\n");
}

/// `(a + b) * c` — explicit parens override precedence. FCS produces
/// `App(*, Paren(App(+, a, b)), c)`. The inner `parse_expr` inside
/// `parse_paren_expr` runs its own Pratt climber to completion, and
/// the outer `*` is picked up *after* the LexFilter-swallowed `)`
/// closes the paren — exercised by the
/// [`Parser::peek_infix_continuation`] RParen gate.
#[test]
fn diff_ast_paren_overrides_precedence() {
    assert_asts_match("(a + b) * c\n");
}

/// `a = b` — `Equals` is the bare `=` token (not an `Op("=")`), which
/// our classifier maps into the INFIX_COMPARE_OP bucket. The op
/// projects to `LongIdent(["="])` via the `OriginalNotation` trivia
/// channel on the FCS side; FCS's `idText` would be `"op_Equality"`,
/// and the diff would diverge if we forgot to unwrap the trivia.
#[test]
fn diff_ast_infix_equals_compare() {
    assert_asts_match("a = b\n");
}

/// `a && b || c` — `||` (BAR_BAR, lbp=10) sits below `&&` (AMP_AMP,
/// lbp=20) per pars.fsy lines 352, 355. FCS produces
/// `App(||, App(&&, a, b), c)`. Confirms left-associativity holds
/// across the precedence boundary as well.
#[test]
fn diff_ast_infix_amp_amp_bar_bar() {
    assert_asts_match("a && b || c\n");
}

/// `a |> f` — `Op("|>")` classifies via the `|` branch into the
/// compare bucket. FCS records `idText = "op_PipeRight"` and trivia
/// `OriginalNotation "|>"`; we keep `|>` directly in the green tree.
#[test]
fn diff_ast_infix_pipe_right() {
    assert_asts_match("a |> f\n");
}

/// `a ** b ** c` — `**` is right-associative per pars.fsy line 366
/// (`%right INFIX_STAR_STAR_OP`). FCS produces `App(**, a, App(**, b, c))`.
/// Pins our INFIX_STAR_STAR_OP precedence shape and right-associativity.
#[test]
fn diff_ast_infix_star_star_right_assoc() {
    assert_asts_match("a ** b ** c\n");
}

/// `a mod b` — the `mod` keyword is `INFIX_STAR_DIV_MOD_OP` per
/// lex.fsl line 970-972 and pars.fsy's precedence table. Same precedence
/// band as `*` and `/`. Without this, our parser ends the expression
/// at `a` and reports `mod` as unexpected. Pins keyword-as-infix.
#[test]
fn diff_ast_infix_mod_keyword() {
    assert_asts_match("a mod b\n");
}

/// `a mod b * c` — `mod` and `*` are in the same INFIX_STAR_DIV_MOD_OP
/// bucket, left-associative. FCS produces `App(*, App(mod, a, b), c)`.
/// Confirms `mod` lives at the right precedence band.
#[test]
fn diff_ast_infix_mod_left_assoc_with_star() {
    assert_asts_match("a mod b * c\n");
}

/// `a $+ b` — `$+` is a custom operator. FCS lex.fsl line 974 (PLUS_MINUS_OP)
/// matches with `$` as `ignored_op_char` prefix and `+` as head; line 978
/// (INFIX_COMPARE_OP) matches with `$` as head and `+` as op_char tail.
/// Both length 2, but fslex's first-rule-wins tie-break favors line 974.
/// Pins our `classify_op_text` behavior on a contended dollar-prefix op.
/// FCS still projects the recovery AST but marks the parse as erroneous.
#[test]
fn diff_ast_infix_dollar_plus() {
    assert_asts_match_fcs_rejects_ours_accepts("a $+ b\n");
}

/// `a %> b` — `%`-prefixed operators (other than bare `%`/`%%`) classify as
/// INFIX_STAR_DIV_MOD_OP per lex.fsl line 972. Confirms multi-char `%`
/// ops are infix at the right precedence band.
#[test]
fn diff_ast_infix_percent_op() {
    assert_asts_match("a %> b\n");
}

/// `a % b` (spaces) — bare `%` IS infix per pars.fsy line 4757
/// (`declExpr PERCENT_OP declExpr`), classified at INFIX_STAR_DIV_MOD
/// level per pars.fsy line 365. With spaces around the `%`, FCS produces
/// the infix App shape `App(INFIX_APP(%, a), b)`.
#[test]
fn diff_ast_bare_percent_with_spaces() {
    assert_asts_match("a % b\n");
}

/// `a %% b` — bare `%%` follows the same rule as `%` (pars.fsy 4757).
/// Symmetric pin to [`diff_ast_bare_percent_with_spaces`].
#[test]
fn diff_ast_bare_double_percent_with_spaces() {
    assert_asts_match("a %% b\n");
}

/// `a $ b` — bare `$` is `DOLLAR` token, which pars.fsy line 359 places
/// at the INFIX_COMPARE_OP precedence band (and line 4725 gives it a
/// `declExpr DOLLAR declExpr` production). Pins the dedicated
/// `Token::Dollar` arm in [`Parser::peek_infix_op`].
#[test]
fn diff_ast_infix_bare_dollar() {
    assert_asts_match("a $ b\n");
}

/// `f - 1` — well-spaced minus is plain infix subtraction. Pins the
/// adjacency gate's "still infix when gap on both sides" branch.
#[test]
fn diff_ast_spaced_minus_is_infix() {
    assert_asts_match("f - 1\n");
}

/// `f-1` — no whitespace on either side. FCS keeps this as plain
/// `MINUS` (infix); the ADJACENT_PREFIX_OP rewrite at LexFilter.fs:2694
/// requires a left gap (`not (prevWasAtomicEnd && lastTokenPos ==
/// startOfThis)`), so no-gap-left short-circuits the rewrite. Pins our
/// adjacency gate's "no left gap → still infix" branch.
#[test]
fn diff_ast_unspaced_minus_is_infix() {
    assert_asts_match("f-1\n");
}

/// `a $! b` — `$!` lexes as `Op("$!")`. fslex first matches the
/// `ignored_op_char*` prefix greedily (eating `$`), leaving head `!`.
/// `!` alone (no following `=`) doesn't match line 978's `!=`
/// head; the head-based rules 970–984 also miss; the fallback in
/// `classify_op_text` finds `$` in the consumed prefix and routes to
/// INFIX_COMPARE_OP — matching fslex's first-rule-wins on the original
/// input where rule 978 (`$` as compare head) beats rule 986 (`!` as
/// prefix). Pins the `$`-in-greedy-prefix fallback path.
/// FCS still projects the recovery AST but marks the parse as erroneous.
#[test]
fn diff_ast_infix_dollar_bang() {
    assert_asts_match_fcs_rejects_ours_accepts("a $! b\n");
}

/// `- x\n` — minusExpr-level prefix MINUS on an identifier. pars.fsy
/// line 5141 (`MINUS minusExpr`) at `expr_prefix_plus_minus` precedence.
/// FCS dispatches through `mkSynPrefix` to `mkSynOperator "~-"`, which
/// strips the leading tilde and emits an `IdentTrivia.OriginalNotation
/// "-"` long-ident; the FCS-side normaliser unwraps the trivia so the
/// diff lines up with our `IDENT_TOK "-"`.
#[test]
fn diff_ast_prefix_minus_ident() {
    assert_asts_match("- x\n");
}

/// `+ x\n` — PLUS_MINUS_OP prefix (lex.fsl PLUS_MINUS_OP family) at
/// minusExpr level. Dispatches through `mkSynPrefix` to `~+`, with
/// `OriginalNotation "+"` trivia.
#[test]
fn diff_ast_prefix_plus_ident() {
    assert_asts_match("+ x\n");
}

/// `%%x\n` — bare `%%` IS a prefix at minusExpr level (pars.fsy
/// `PERCENT_OP minusExpr`). With no whitespace it's unambiguously
/// prefix. FCS emits `App(~%%, x)` via `mkSynPrefix`.
#[test]
fn diff_ast_prefix_double_percent_ident() {
    assert_asts_match("%%x\n");
}

/// `!+ x\n` — `!+` is a `PREFIX_OP` per lex.fsl (any `!`-prefixed op
/// other than `!=`-headed). pars.fsy puts it at atomicExpr level
/// (`PREFIX_OP atomicExpr`). FCS goes through `mkSynPrefix` →
/// `~!+` mangled to `op_BangPlus` with `OriginalNotation "!+"`.
#[test]
fn diff_ast_prefix_bang_plus_ident() {
    assert_asts_match("!+ x\n");
}

/// `~~~x\n` — `~~~` is a PREFIX_OP (`~`-headed). Compiles to
/// `op_LogicalNot` with `OriginalNotation "~~~"`. Pins the
/// multi-tilde prefix-strip rule in `mkSynOperator`.
#[test]
fn diff_ast_prefix_triple_tilde_ident() {
    assert_asts_match("~~~x\n");
}

/// `&x\n` — `AMP minusExpr` for byref address-of. FCS emits
/// `SynExpr.AddressOf(isByref=true, x, opRange, range)`; we emit an
/// `ADDRESS_OF_EXPR` node.
#[test]
fn diff_ast_address_of_byref_ident() {
    assert_asts_match("&x\n");
}

/// `&&x\n` — `AMP_AMP minusExpr` for nativeptr address-of. Symmetric
/// to [`diff_ast_address_of_byref_ident`] but `isByref=false`.
#[test]
fn diff_ast_address_of_nativeptr_ident() {
    assert_asts_match("&&x\n");
}

/// `f -x\n` — adjacent `-` in arg position triggers LexFilter's
/// `ADJACENT_PREFIX_OP` rewrite (LexFilter.fs:2694), which feeds
/// pars.fsy's `argExpr: ADJACENT_PREFIX_OP atomicExpr`. Result:
/// `App(f, App(~-, x))`. Pins the arg-position prefix path.
#[test]
fn diff_ast_adjacent_minus_in_arg_position() {
    assert_asts_match("f -x\n");
}

/// `- 1 + 2\n` — well-spaced prefix MINUS, then infix `+`. The
/// `MINUS minusExpr` rule reduces eagerly once the `+` lookahead is
/// seen (the `+` isn't a valid continuation inside `minusExpr`),
/// promoting `-1` to `declExpr` before the outer `declExpr PLUS_MINUS_OP
/// declExpr` rule fires. Result against the FCS oracle: `App(+, App(~-,
/// 1), 2)` — i.e. `(-1) + 2`, not `-(1+2)`. Pins the precedence
/// ordering against a once-suspected (and refuted by oracle) inversion.
#[test]
fn diff_ast_prefix_minus_then_plus() {
    assert_asts_match("- 1 + 2\n");
}

/// `- a * b\n` — prefix MINUS followed by `*`. Same reduce-eagerly
/// reasoning as the `+` case: `*` doesn't extend a `minusExpr`, so
/// the `MINUS minusExpr` rule reduces with `a` as its operand, then
/// the outer `declExpr STAR declExpr` rule combines `(-a)` with `b`.
/// FCS shape: `App(*, App(~-, a), b)` — i.e. `(-a) * b`. Tighter
/// pin than the `+` case because `*` sits one precedence band higher
/// than `+` in pars.fsy line 365 yet still doesn't pull `-` rightward.
#[test]
fn diff_ast_prefix_minus_then_star() {
    assert_asts_match("- a * b\n");
}

/// `- - 1\n` — double prefix. With both `-`s spaced (no
/// adjacency), neither participates in sign-folding; we emit
/// `App(~-, App(~-, 1))`.
#[test]
fn diff_ast_double_prefix_minus() {
    assert_asts_match("- - 1\n");
}

/// `a - -b\n` — infix subtraction with adjacent prefix `-b` on the
/// RHS. Adjacency on the right of `-` matters: the second `-` is
/// adjacent to `b`, so it's ADJACENT_PREFIX_OP. Result:
/// `App(-, a, App(~-, b))` at the outer infix layer.
#[test]
fn diff_ast_infix_minus_with_adjacent_prefix_rhs() {
    assert_asts_match("a - -b\n");
}

/// `?+ x\n` — `?+` is `PLUS_MINUS_OP` per lex.fsl rule 974
/// (`ignored_op_char* ('+'|'-') op_char*` where `ignored_op_char =
/// '.' | '$' | '?'`). FCS's `IsValidPrefixOperatorUse`
/// (`PrettyNaming.fs:629-630`) names `?+`/`?-` explicitly, so the
/// minusExpr prefix path succeeds without an "invalid prefix
/// operator" diagnostic. Shape: `App(~?+, x)`. Pins the carve-out
/// for the `?`-prefixed PLUS_MINUS_OP variants alongside the bare
/// `+`/`-`/`+.`/`-.` cases.
#[test]
fn diff_ast_prefix_qmark_plus_ident() {
    assert_asts_match("?+ x\n");
}

/// `?- x\n` — sibling of [`diff_ast_prefix_qmark_plus_ident`]:
/// `?-` is `PLUS_MINUS_OP` and is named by
/// `IsValidPrefixOperatorUse`. Shape: `App(~?-, x)`.
#[test]
fn diff_ast_prefix_qmark_minus_ident() {
    assert_asts_match("?- x\n");
}

/// Phase 5.3 — `if a then 1 else if b then 2 else 3`: the same-line
/// `else if` merge. LexFilter rewrites the adjacent `else` + `if`
/// into a single `Token::Elif` covering both keywords. FCS records
/// the merge by clearing `isElif` (vs. the bare `elif`'s `isElif =
/// true`) but the structural shape is the same — both project to
/// `IfThenElse(a, 1, Some(IfThenElse(b, 2, Some(3))))`.
#[test]
fn diff_ast_merged_else_if_with_trailing_else() {
    assert_asts_match("if a then 1 else if b then 2 else 3\n");
}

/// Phase 5.3 — `if a then 1 else (* c *) if b then 2 else 3`: the
/// merged `else if` form with a block comment between the keywords.
/// LexFilter's lookahead skips block comments when deciding to merge,
/// so the resulting `Token::Elif` spans the entire run including the
/// comment. The structural projection is unchanged
/// (`IfThenElse(a, 1, Some(IfThenElse(b, 2, Some(3))))`); this test
/// pins that the parse stays clean once the keywords are emitted as
/// distinct `ELSE_TOK` / `IF_TOK` tokens with the comment draining
/// between them as its own `BLOCK_COMMENT` trivia.
#[test]
fn diff_ast_merged_else_if_with_block_comment_between_keywords() {
    assert_asts_match("if a then 1 else (* c *) if b then 2 else 3\n");
}

// ---------------------------------------------------------------------------
// Sign-folding (`LexFilter.fs:2694`): an adjacent `+`/`-` folds into the
// following numeric literal token, so FCS produces `Const(±v)` rather than
// `App(~±, Const v)`. See `crates/cst/src/parser/sign_fold.rs` and the
// "Sign-folding" entry in `docs/fcs-divergences.md`.
// ---------------------------------------------------------------------------

/// `-1` — adjacent `-` before a decimal int folds to `SynConst.Int32 -1`,
/// not `App(~-, 1)`. The headline sign-fold case.
#[test]
fn diff_ast_fold_neg_int() {
    assert_asts_match("-1\n");
}

/// `+1` — adjacent `+` folds too, dropping the sign: `SynConst.Int32 1`
/// (FCS keeps the value `v` unchanged for `+`).
#[test]
fn diff_ast_fold_pos_int() {
    assert_asts_match("+1\n");
}

/// `-1.5` — adjacent `-` before a double folds to `SynConst.Double -1.5`.
#[test]
fn diff_ast_fold_neg_double() {
    assert_asts_match("-1.5\n");
}

/// `-1e3` — exponent-form double folds.
#[test]
fn diff_ast_fold_neg_double_exponent() {
    assert_asts_match("-1e3\n");
}

/// `-1.0f` — single-precision (`IEEE32`) folds to `SynConst.Single -1.0`.
#[test]
fn diff_ast_fold_neg_single() {
    assert_asts_match("-1.0f\n");
}

/// `-0x40490fdblf` — hex-bit-pattern single folds by bit-casting first, then
/// applying unary negation (`0x40490fdb` → `0xc0490fdb`).
#[test]
fn diff_ast_fold_neg_hex_single() {
    assert_asts_match("-0x40490fdblf\n");
}

/// `+0x40490fdblf` — a folded plus on a hex-bit-pattern single is a no-op.
#[test]
fn diff_ast_fold_pos_hex_single() {
    assert_asts_match("+0x40490fdblf\n");
}

/// `-0x4024000000000000LF` — hex-bit-pattern double folds by bit-casting first,
/// then applying unary negation.
#[test]
fn diff_ast_fold_neg_hex_double() {
    assert_asts_match("-0x4024000000000000LF\n");
}

/// `-1m` — decimal folds to `SynConst.Decimal -1`.
#[test]
fn diff_ast_fold_neg_decimal() {
    assert_asts_match("-1m\n");
}

/// `-1.5m` — fractional decimal folds, preserving scale (`-1.5`).
#[test]
fn diff_ast_fold_neg_decimal_fractional() {
    assert_asts_match("-1.5m\n");
}

/// `-1I` — bignum folds to `SynConst.UserNum("-1", "I")` (FCS prepends `-`
/// to the value string).
#[test]
fn diff_ast_fold_neg_bignum() {
    assert_asts_match("-1I\n");
}

/// `-128y` — signed byte at its `MinValue`. FCS's `isInt8BadMax` lexer arm +
/// the `plus && bad` fold accept `-128y` though `128y` alone overflows.
#[test]
fn diff_ast_fold_neg_sbyte_min() {
    assert_asts_match("-128y\n");
}

/// `-32768s` — int16 `MinValue`, same `isInt16BadMax` rescue.
#[test]
fn diff_ast_fold_neg_int16_min() {
    assert_asts_match("-32768s\n");
}

/// `-1L` — int64 folds.
#[test]
fn diff_ast_fold_neg_int64() {
    assert_asts_match("-1L\n");
}

/// `-1n` — nativeint folds.
#[test]
fn diff_ast_fold_neg_nativeint() {
    assert_asts_match("-1n\n");
}

/// `-0xFF` — hex int folds by negating the (bit-reinterpreted) value:
/// `0xFF` = 255, so `-0xFF` = `SynConst.Int32 -255`.
#[test]
fn diff_ast_fold_neg_hex() {
    assert_asts_match("-0xFF\n");
}

/// `-0b101` / `-0o17` — binary/octal int bodies fold the same way.
#[test]
fn diff_ast_fold_neg_bin_oct() {
    assert_asts_match("-0b101\n");
    assert_asts_match("-0o17\n");
}

/// `-2147483648` — int32 `MinValue`. The headline correctness case: FCS
/// accepts it (no diagnostic) via `isInt32BadMax` + the `-` clearing the
/// `bad` flag, where the bare `2147483648` overflows. Pre-fold we emitted a
/// spurious "outside 32-bit signed range" error here.
#[test]
fn diff_ast_fold_int32_min() {
    assert_asts_match("-2147483648\n");
}

/// `-9223372036854775808L` — int64 `MinValue`, the `isInt64BadMax` rescue.
#[test]
fn diff_ast_fold_int64_min() {
    assert_asts_match("-9223372036854775808L\n");
}

/// `f -1` — adjacent `-` in *argument* position folds, so the literal is a
/// single atomic arg: `App(f, Const -1)`. (Contrast `f -x` where the operand
/// is an ident, not a literal, so no fold — that's
/// `diff_ast_adjacent_minus_in_arg_position`.)
#[test]
fn diff_ast_fold_neg_in_arg_position() {
    assert_asts_match("f -1\n");
}

/// `(-1)` — `-` is the first token of the paren body, so there's no
/// atomic-end token to its left: it folds to `Paren(Const -1)`.
#[test]
fn diff_ast_fold_neg_in_parens() {
    assert_asts_match("(-1)\n");
}

/// `1 +2` — the classic F# gotcha: gap-left, no-gap-right `+` folds to a
/// signed literal, turning `1 +2` into the *application* `App(1, Const 2)`
/// rather than infix addition. Pins that the fold fires in arg position
/// after an atomic literal with a left gap.
#[test]
fn diff_ast_fold_pos_makes_application() {
    assert_asts_match("1 +2\n");
}

/// `match … with -1 -> …` — sign-folding applies in *pattern* position too
/// (FCS folds at the token layer, before the grammar). Pre-fold this
/// cascaded into a parse-error storm; now `-1` is `SynPat.Const(Int32 -1)`.
#[test]
fn diff_ast_fold_neg_in_match_pattern() {
    assert_asts_match("match x with\n| -1 -> a\n| _ -> b\n");
}

/// `-1uy` — unsigned byte is **not** in FCS's fold set, so the sign stays a
/// prefix op: `App(~-, Const(Byte 1))`. Pins that the fold excludes unsigned
/// suffixed ints.
#[test]
fn diff_ast_unsigned_suffix_does_not_fold() {
    assert_asts_match("-1uy\n");
}

/// `2147483648` — bare (unsigned) int32 `MaxValue + 1` overflows: FCS emits
/// a diagnostic and recovers `SynConst.Int32 MinValue` (its `isInt32BadMax`
/// fallback). Both sides error and agree on the recovered value.
#[test]
fn diff_ast_bare_int32_overflow_errors() {
    assert_asts_match_allow_errors("2147483648\n");
}

/// `+2147483648` — a folded `+` does **not** clear the `bad` flag, so this
/// still overflows. FCS recovers `SynConst.Int32 MinValue` + a diagnostic;
/// we match the recovered value and also error.
#[test]
fn diff_ast_fold_pos_int32_overflow_errors() {
    assert_asts_match_allow_errors("+2147483648\n");
}

/// `-0128y` — a leading-zero spelling of int8 `MinValue`. FCS's int8 rescue
/// (`isInt8BadMax`) is *value*-based (`lex.fsl:26`), so the leading zero is
/// still accepted: `SynConst.SByte -128`. Pins that int8/int16 rescue (unlike
/// int32/int64) is spelling-insensitive.
#[test]
fn diff_ast_fold_sbyte_min_leading_zero() {
    assert_asts_match("-0128y\n");
}

/// `Some -1` — sign-folding in a *constructor-argument* pattern position.
/// The folded `-1` is a curried arg of the `Some` long-ident pattern. Pins
/// that the function-form sweep recognises a folded literal whose raw
/// lookahead still shows `Op("-")`.
#[test]
fn diff_ast_fold_neg_in_ctor_arg_pattern() {
    assert_asts_match("match x with\n| Some -1 -> a\n| _ -> b\n");
}

/// `1, -1` — folded literal as a *tuple element* after `,` (reached via the
/// `climb_pat_tail` comma loop → `emit_pat_atom`, a raw-gated site).
#[test]
fn diff_ast_fold_neg_in_tuple_pattern() {
    assert_asts_match("match x with\n| 1, -1 -> a\n| _ -> b\n");
}

/// `1 :: -1 :: []` — folded literal as a `::` *rhs* (raw-gated via
/// `emit_pat_atom`).
#[test]
fn diff_ast_fold_neg_in_cons_pattern() {
    assert_asts_match("match x with\n| 1 :: -1 :: [] -> a\n| _ -> b\n");
}

/// `[ -1; -2 ]` — folded literals as list-pattern elements.
#[test]
fn diff_ast_fold_neg_in_list_pattern() {
    assert_asts_match("match x with\n| [ -1; -2 ] -> a\n| _ -> b\n");
}

/// `Some -0x40490fdblf` — the folded-XIEEE token must be recognised by the
/// pattern continuation gates whose raw lookahead still sees the pre-fold `-`.
#[test]
fn diff_ast_fold_neg_hex_single_in_ctor_arg_pattern() {
    assert_asts_match("match x with\n| Some -0x40490fdblf -> a\n| _ -> b\n");
}

/// `let f -1 = 0` — a folded constant as a *function-form binding* argument
/// pattern (`SynPat.LongIdent(f, [Const -1])`). Pins the promotion + sweep
/// raw lookaheads recognising the fold.
#[test]
fn diff_ast_fold_neg_function_form_arg() {
    assert_asts_match("let f -1 = 0\n");
}

/// `let f (x) -1 = 0` — a paren arg followed by a folded literal arg. The
/// inner lowercase `x` must stay `SynPat.Named` (its raw lookahead is the
/// swallowed `)`, not a fold sign), while the outer `f` is function-form
/// with args `(x)` and `Const -1`. Pins that the folded-arg promotion keeps
/// the swallowed-`)` guard.
#[test]
fn diff_ast_fold_arg_after_paren_arg() {
    assert_asts_match("let f (x) -1 = 0\n");
}

// ----------------------------------------------------------------------------
// Parenthesised operator-values: `( op )` as a value (FCS's `opName`
// production, `pars.fsy:6793`). FCS projects each to `SynExpr.LongIdent`
// (single segment, the mangled `op_*` name) carrying an
// `IdentTrivia.OriginalNotationWithParen` whose text is the source spelling,
// which the FCS-side normaliser unwraps to match our green tree's raw op
// token. `Checked.(-)` folds the operator onto the long-ident as a trailing
// segment via `mkSynDot` (`SyntaxTreeOps.fs:533`).
// ----------------------------------------------------------------------------

/// `(+)` — bare addition operator-value. FCS: `LongIdent(["op_Addition"])`
/// with `OriginalNotationWithParen "+"`; both sides project to
/// `LongIdent(["+"])`.
#[test]
fn diff_ast_paren_op_value_plus() {
    assert_asts_match("(+)\n");
}

/// `(-)` — bare subtraction operator-value (the `-` is `Op("-")`, the same
/// token that is prefix-able, so the operator-value reinterpretation must
/// fire *before* the prefix-application path).
#[test]
fn diff_ast_paren_op_value_minus() {
    assert_asts_match("(-)\n");
}

/// `( + )` — spaced form. The interior trivia drains into the `LONG_IDENT`
/// for losslessness; the projection is identical to the unspaced `(+)`.
#[test]
fn diff_ast_paren_op_value_plus_spaced() {
    assert_asts_match("( + )\n");
}

/// `(|>)` — an *infix-only* operator-value (`|>` cannot lead a paren-expr
/// body, so this exercises the broadened `(`-after expr-start gate).
#[test]
fn diff_ast_paren_op_value_pipe_right() {
    assert_asts_match("(|>)\n");
}

/// `(=)` — the bare `=` token (`Token::Equals`, not `Op("=")`).
#[test]
fn diff_ast_paren_op_value_equals() {
    assert_asts_match("(=)\n");
}

/// `(<)` / `(>)` — the bare `<` / `>` tokens (`Token::Less`/`Greater`).
#[test]
fn diff_ast_paren_op_value_less() {
    assert_asts_match("(<)\n");
}

/// `(>)` companion to [`diff_ast_paren_op_value_less`].
#[test]
fn diff_ast_paren_op_value_greater() {
    assert_asts_match("(>)\n");
}

/// `(&&)` / `(||)` / `(:=)` / `($)` / `(&)` — the remaining fixed
/// operator-name tokens FCS accepts.
#[test]
fn diff_ast_paren_op_value_amp_amp() {
    assert_asts_match("(&&)\n");
}

/// `(||)` companion.
#[test]
fn diff_ast_paren_op_value_bar_bar() {
    assert_asts_match("(||)\n");
}

/// `(:=)` companion.
#[test]
fn diff_ast_paren_op_value_colon_equals() {
    assert_asts_match("(:=)\n");
}

/// `($)` companion (`Token::Dollar`).
#[test]
fn diff_ast_paren_op_value_dollar() {
    assert_asts_match("($)\n");
}

/// `(&)` — single ampersand is `op_Amp` in FCS (an operator-value), even
/// though `&` is otherwise the address-of prefix.
#[test]
fn diff_ast_paren_op_value_amp() {
    assert_asts_match("(&)\n");
}

/// `(..)` — the range operator as a value (`op_Range`). Distinct from the
/// open-ended range `(..3)` (which is `Paren(IndexRange(None, Some 3))`) — the
/// operator-value form requires the `)` to immediately follow the `..`.
#[test]
fn diff_ast_paren_op_value_range() {
    assert_asts_match("(..)\n");
}

/// `(*)` — glued multiply operator-value (the lexer's dedicated
/// `LParenStarRParen` token). Contrast with the spaced `( * )`, which FCS
/// parses as the whole-dimension wildcard `Paren(IndexRange(None, None))`.
#[test]
fn diff_ast_paren_op_value_star_glued() {
    assert_asts_match("(*)\n");
}

/// `( * )` — spaced star stays the wildcard `Paren(IndexRange(None, None))`,
/// *not* an operator-value (regression guard for the `Op("*")` exclusion).
#[test]
fn diff_ast_paren_spaced_star_is_wildcard() {
    assert_asts_match("( * )\n");
}

/// `(.())` — the index-get funky operator name as a bare operator-*value*
/// expression (FCS's `identExpr: opName`, `op_ArrayLookup`). The clean funky
/// subset admitted in pattern/member position (`is_clean_funky_operator_name`)
/// rides the same shared `is_paren_operator_name` predicate, so it is an
/// operator-value here too; guards that the expression path stays FCS-faithful.
#[test]
fn diff_ast_paren_op_value_funky_index_get() {
    assert_asts_match("(.())\n");
}

/// `(.()<-)` — the index-set funky operator-value (`op_ArrayAssign`).
#[test]
fn diff_ast_paren_op_value_funky_index_set() {
    assert_asts_match("(.()<-)\n");
}

/// `(.[])` — the dot-bracket funky operator-value (`op_DotLBrackRBrack`).
#[test]
fn diff_ast_paren_op_value_funky_dot_bracket() {
    assert_asts_match("(.[])\n");
}

/// `let f = List.map (+)` — operator-value in (spaced) application-argument
/// position.
#[test]
fn diff_ast_paren_op_value_as_app_arg() {
    assert_asts_match("let f = List.map (+)\n");
}

/// `f(+)` — operator-value as a high-precedence (adjacent) paren-app
/// argument: `App(Atomic, f, (+))`.
#[test]
fn diff_ast_paren_op_value_adjacent_app_arg() {
    assert_asts_match("f(+)\n");
}

/// `Checked.(-)` — the qualified operator-value: `mkSynDot` folds the
/// operator onto the `Checked` long-ident, giving
/// `LongIdent(["Checked"; "op_Subtraction"])` with the trailing segment
/// carrying `OriginalNotationWithParen "-"`.
#[test]
fn diff_ast_qualified_op_value_checked_minus() {
    assert_asts_match("Checked.(-)\n");
}

/// `Operators.(+)` — qualified addition operator-value.
#[test]
fn diff_ast_qualified_op_value_operators_plus() {
    assert_asts_match("Operators.(+)\n");
}

/// `let f a = Checked.(-) a 1` — the qualified operator-value applied to two
/// arguments (the motivating real-world form): `App(App(Checked.(-), a), 1)`.
#[test]
fn diff_ast_qualified_op_value_applied() {
    assert_asts_match("let f a = Checked.(-) a 1\n");
}

/// `(id 1).(+)` — qualified operator-value off a *non-ident* head, which
/// FCS lowers to `DotGet(Paren(App(id, 1)), ["op_Addition"])`.
#[test]
fn diff_ast_qualified_op_value_non_ident_head() {
    assert_asts_match("(id 1).(+)\n");
}

/// `id ((+))` — a *paren-wrapped* operator-value as an application argument.
/// The inner `(+)` is the operator-value; the outer `(…)` is an ordinary
/// `Paren`. Unlike a bare `( op )` argument, this is valid in
/// `atomicExprAfterType` contexts too, because the head token is the outer
/// `(` (a paren-expression), so the operator-value exclusion does not apply.
#[test]
fn diff_ast_paren_wrapped_op_value_app_arg() {
    assert_asts_match("id ((+))\n");
}

/// `(+).GetType` — a member access *off* a bare operator-value. FCS's
/// `mkSynDot` appends `.GetType` onto the `SynExpr.LongIdent` that the
/// operator-value produced, so the whole thing is a single
/// `LongIdent(["op_Addition"; "GetType"])` (→ `["+"; "GetType"]`), **not** a
/// `DotGet`. The bare operator-value head must therefore fold trailing
/// `.member` segments into its own long-ident.
#[test]
fn diff_ast_op_value_dot_member() {
    assert_asts_match("(+).GetType\n");
}

/// `Operators.( * )` — the *spaced* star as a dot-qualified operator-value.
/// FCS routes `.( * )` through `atomicExprQualification`'s
/// `LPAREN typedSequentialExpr rparen` arm, matches the `IndexRange(None,
/// None)` wildcard body, and rewrites it to `op_Multiply`. So this is
/// `LongIdent(["Operators"; "op_Multiply"])` (→ `["Operators"; "*"]`) — even
/// though a *bare* `( * )` is the wildcard.
#[test]
fn diff_ast_qualified_op_value_spaced_star() {
    assert_asts_match("Operators.( * )\n");
}

/// `(id 1).( * )` — spaced-star operator-value qualification off a non-ident
/// head: `DotGet(Paren(App(id, 1)), ["op_Multiply"])`.
#[test]
fn diff_ast_qualified_op_value_spaced_star_non_ident_head() {
    assert_asts_match("(id 1).( * )\n");
}

/// `(*).GetType` — trailing member off the glued multiply operator-value
/// (`LongIdent(["op_Multiply"; "GetType"])`).
#[test]
fn diff_ast_glued_star_op_value_dot_member() {
    assert_asts_match("(*).GetType\n");
}

/// `f(+).Bar` — an operator-value as a high-precedence (adjacent) paren-app
/// argument, followed by a member access. The `.Bar` must bind to the *whole*
/// application, not the argument: `DotGet(App(Atomic, f, (+)), ["Bar"])`. The
/// `(+)` argument is parsed head-only, so it does **not** fold `.Bar` into its
/// own long-ident (contrast the bare `(+).Bar` =
/// [`diff_ast_op_value_dot_member`]).
#[test]
fn diff_ast_op_value_hpa_arg_then_member() {
    assert_asts_match("f(+).Bar\n");
}

/// `f (+).Bar` — spaced application of `f` to the bare operator-value member
/// access `(+).Bar`. Here the `(+).Bar` *is* a single `LongIdent(["+";"Bar"])`
/// argument (no HPA marker on the spaced `(`), so the result is
/// `App(f, LongIdent(["+"; "Bar"]))`.
#[test]
fn diff_ast_op_value_spaced_arg_member_folds() {
    assert_asts_match("f (+).Bar\n");
}
