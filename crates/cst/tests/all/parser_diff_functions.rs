//! Differential test (`parser::parse` vs FCS): `fun` lambdas and `function`
//! pattern-matching lambdas. Split out of the former monolithic
//! `parser_diff.rs`.

use crate::common::{assert_asts_match, assert_asts_match_allow_errors};

/// Phase 5.2 — `fun x -> x`: the smallest valid `fun`-lambda. Exercises
/// `Virtual::Fun` dispatch in `parse_minus_expr`, a single atomic
/// parameter pattern (`NamedPat`), the `->` token, and a trivial body.
/// FCS projects to `SynExpr.Lambda` with one arg and an identifier body.
#[test]
fn diff_ast_fun_lambda_single_arg() {
    assert_asts_match("fun x -> x\n");
}

/// Phase 5.2 — `fun x y -> x + y`: a curried two-argument lambda with a
/// non-trivial body that exercises Pratt's `+` infix layer. FCS's
/// curried encoding is `Lambda(_, _, [x], Lambda(_, _, [y], App(App(+,
/// x), y)))` plus `parsedData = Some([[x; y]], +-app)`; our flat
/// `FUN_EXPR` normalises to the `parsedData` view.
#[test]
fn diff_ast_fun_lambda_curried_two_args() {
    assert_asts_match("fun x y -> x + y\n");
}

/// Phase 5.2 — `fun _ -> 0`: wildcard parameter. `WildcardPat` is
/// `atomicPattern`-eligible (`pars.fsy:3922`), so it shows up directly
/// as a `FunExpr.args()` element rather than wrapped in a `ParenPat`.
#[test]
fn diff_ast_fun_lambda_wildcard_arg() {
    assert_asts_match("fun _ -> 0\n");
}

/// Phase 5.2 — `fun () -> 1`: the unit-parameter form. FCS represents
/// `()` as `Paren(Const(Unit))` at the pattern level because the unit
/// pat only ever reaches the pattern grammar wrapped in parens
/// (`pars.fsy:3929`). Verifies the paren+const-unit round-trip through
/// our lambda's `atomicPattern` path.
#[test]
fn diff_ast_fun_lambda_unit_arg() {
    assert_asts_match("fun () -> 1\n");
}

/// Phase 5.2 — `fun (x, y) -> x`: tuple-paren parameter. `TuplePat`
/// isn't atomic, so the user must write parens around it; our parser
/// reaches it via `atomicPattern → ParenPat → inner pat → TuplePat`.
/// FCS's flat `parsedData` view matches one paren-wrapped tuple pat.
#[test]
fn diff_ast_fun_lambda_tuple_paren_arg() {
    assert_asts_match("fun (x, y) -> x\n");
}

/// Phase 5.2 — `let f = fun y -> y`: lambda on the RHS of a let
/// binding. Exercises the interaction between the let's RHS-block and
/// the lambda's trailing virtual-close cascade: the
/// `Virtual::RightBlockEnd`/`Virtual::End` for the lambda must be
/// drained *before* the let attempts its own block close, or the
/// outer let frame loses its scaffolding.
#[test]
fn diff_ast_fun_lambda_as_let_rhs() {
    assert_asts_match("let f = fun y -> y\n");
}

/// Phase 5.2 — `fun x -> 1, 2`: the body slot is `typedSeqExprBlockR`,
/// which sits below `COMMA` in FCS's precedence (`pars.fsy:323`). So
/// the comma binds *into* the body, producing `Lambda(x, Tuple([1; 2]))`
/// rather than `Tuple([Lambda(x, 1); 2])`.
#[test]
fn diff_ast_fun_lambda_tuple_body_shift() {
    assert_asts_match("fun x -> 1, 2\n");
}

/// Phase 5.2 — `fun x -> if x then 1 else 2`: an `if/then/else` body.
/// Confirms the lambda's body accepts a keyword-led form via
/// `parse_expr`'s usual dispatch chain, and that the inner if's own
/// virtual-close cascade doesn't accidentally short-circuit the
/// surrounding lambda close.
#[test]
fn diff_ast_fun_lambda_if_body() {
    assert_asts_match("fun x -> if x then 1 else 2\n");
}

/// Phase 5.2 — `(fun x -> x)`: parenthesised lambda. Tests the LParen
/// dispatch in `peek_is_expr_start`: the raw-stream lookahead past `(`
/// sees `Token::Fun`, which must register as an expr-starter via
/// `raw_starts_minus_expr`. The trailing `)` must remain a sibling of
/// `FUN_EXPR` inside the `PAREN_EXPR`, not get drained into the lambda
/// by its virtual-close cascade.
#[test]
fn diff_ast_fun_lambda_in_parens() {
    assert_asts_match("(fun x -> x)\n");
}

/// Phase 5.2 — `(fun x -> x) 1`: parenthesised lambda used as the
/// callee of an application. Beyond what `diff_ast_fun_lambda_in_parens`
/// covers: the trailing raw `1` must reach the app-arg slot. That
/// requires the lambda's virtual-close to be zero-width (not draining
/// raw past it) and the surrounding paren-expr to own the `)`.
#[test]
fn diff_ast_fun_lambda_paren_app_callee() {
    assert_asts_match("(fun x -> x) 1\n");
}

/// Phase 5.2 — `List.map (fun x -> x) xs`: parenthesised lambda as an
/// app argument between two other arguments. Pins the same set of
/// invariants as `diff_ast_fun_lambda_paren_app_callee` but in
/// app-arg position rather than callee — the surrounding app-arg loop
/// must continue past the closing `)` and pick up `xs`.
#[test]
fn diff_ast_fun_lambda_as_app_arg() {
    assert_asts_match("List.map (fun x -> x) xs\n");
}

/// Phase 5.2 — `fun x ->\n    a\n    b`: multi-statement lambda body.
/// The `->` pushes a one-sided SeqBlock whose subsequent statements are
/// separated by `Virtual::BlockSep`. FCS wraps the body in
/// `SynExpr.Sequential(SuppressNeither, a, b)`; our parser must mirror
/// that by looping on `BlockSep` inside `parse_fun_expr` and wrapping
/// multi-statement bodies in `SEQUENTIAL_EXPR`.
#[test]
fn diff_ast_fun_lambda_multi_statement_body() {
    assert_asts_match("fun x ->\n    a\n    b\n");
}

/// Phase 5.2 — `fun x -> fun y -> y\nz\n`: a lambda whose body is itself
/// a lambda, followed by a sibling top-level decl. LexFilter emits the
/// inner and outer `RightBlockEnd`/`End` close pairs consecutively at
/// the same source position; `parse_fun_expr`'s close-virtual drain
/// must consume exactly the *one* pair belonging to its own frame and
/// leave the enclosing lambda's close pair for that lambda's own
/// drain. Otherwise the inner lambda steals the outer's close, the
/// outer treats the subsequent `BlockSep` + `z` as another statement
/// in its body, and the top-level decl count drops from two to one.
#[test]
fn diff_ast_fun_lambda_nested_lambda_body_with_sibling() {
    assert_asts_match("fun x -> fun y -> y\nz\n");
}

/// Phase 5.3 — `fun 0 -> 1`: a constant-literal parameter. A `Const`
/// pat isn't `SynSimplePat`-eligible, so FCS's `SimplePatOfPat`
/// (`SyntaxTreeOps.fs:327`) replaces it with a compiler-generated
/// `_arg1` simple pat and wraps the body in a synthetic
/// `match _arg1 with 0 -> 1`. The `parsedData[1]` cache holds that
/// lowered body, so our `normalise_fun` must reproduce the scaffold.
#[test]
fn diff_ast_fun_lambda_const_arg() {
    assert_asts_match("fun 0 -> 1\n");
}

/// Phase 5.3 — `fun 0 1 -> 2`: two const params. `SynArgNameGenerator`
/// is shared across the whole binding and `PushCurriedPatternsToExpr`
/// folds the args right-to-left (`SyntaxTreeOps.fs:425`), so the
/// rightmost `1` claims `_arg1` and the leftmost `0` claims `_arg2`.
/// The lowered body nests: `match _arg2 with 0 -> match _arg1 with 1 -> 2`.
#[test]
fn diff_ast_fun_lambda_two_const_args() {
    assert_asts_match("fun 0 1 -> 2\n");
}

/// Phase 5.3 — `fun _ 0 -> 1`: wildcard left of a const. The wildcard
/// still consumes a counter slot (`SimplePatOfPat` falls into the
/// catch-all and calls `New()`) but produces no match wrapper
/// (`fn = None` for `SynPat.Wild`, `SyntaxTreeOps.fs:355`). Right-to-left
/// the const claims `_arg1`, the wildcard then claims `_arg2`, so the
/// only scaffold is `match _arg1 with 0 -> 1`.
#[test]
fn diff_ast_fun_lambda_wildcard_then_const() {
    assert_asts_match("fun _ 0 -> 1\n");
}

/// Phase 5.3 — `fun 0 _ -> 1`: const left of a wildcard. Mirror of
/// `diff_ast_fun_lambda_wildcard_then_const`: the rightmost wildcard
/// burns `_arg1`, so the const's scaffold scrutinises `_arg2` —
/// `match _arg2 with 0 -> 1`. Pins that a trailing wildcard still
/// advances the shared counter.
#[test]
fn diff_ast_fun_lambda_const_then_wildcard() {
    assert_asts_match("fun 0 _ -> 1\n");
}

/// Phase 5.3 — `fun x 0 -> 1`: a named param left of a const. A
/// `SynPat.Named` is already simple (`SyntaxTreeOps.fs:319`), so it
/// neither consumes a counter slot nor scaffolds — it keeps its source
/// name `x`. The const therefore claims `_arg1`: `match _arg1 with 0 -> 1`.
#[test]
fn diff_ast_fun_lambda_named_then_const() {
    assert_asts_match("fun x 0 -> 1\n");
}

/// Phase 5.3 — `fun null -> 0`: a `null` parameter. `SynPat.Null` is
/// non-simple, so it lowers exactly like a const: `_arg1` plus
/// `match _arg1 with null -> 0`. Confirms the `Null` clause pattern
/// round-trips and that the clause `when` slot serialises as JSON `null`
/// on the FCS side.
#[test]
fn diff_ast_fun_lambda_null_arg() {
    assert_asts_match("fun null -> 0\n");
}

/// Phase 5.3 — `fun X -> X`: a nullary uppercase identifier parameter.
/// Since #150, `X` parses as `LongIdentPat` (a possibly-nullary union
/// case), which isn't a `SynSimplePat`. FCS's `SimplePatOfPat` special
/// arm (`SyntaxTreeOps.fs:332`) claims a counter slot via an
/// `altNameRefCell` but keeps the *original* name as the synthetic
/// scrutinee, so the body lowers to `match X with X -> X` — the
/// scrutinee is `SynExpr.LongIdent ["X"]`, not `Ident _arg1`. The
/// `_arg1` lives only in the elided alt-name cell.
#[test]
fn diff_ast_fun_lambda_longident_arg() {
    assert_asts_match("fun X -> X\n");
}

/// Phase 5.3 — `fun None -> 0`: the practically-important nullary union
/// case at lambda-arg position. Same lowering as
/// `diff_ast_fun_lambda_longident_arg`: `match None with None -> 0`.
#[test]
fn diff_ast_fun_lambda_nullary_union_arg() {
    assert_asts_match("fun None -> 0\n");
}

/// Phase 5.3 — `fun 0 X -> 1`: a const left of a nullary-LongIdent. The
/// LongIdent still *consumes* a counter slot even though its scrutinee
/// is the original name, so right-to-left the LongIdent claims `_arg1`
/// (invisibly) and the const claims `_arg2`. Pins that the LongIdent
/// slot-burn shifts the const's `_argN`:
/// `match _arg2 with 0 -> match X with X -> 1`.
#[test]
fn diff_ast_fun_lambda_const_then_longident() {
    assert_asts_match("fun 0 X -> 1\n");
}

/// Phase 5.3 — `fun X Y -> 0`: two nullary-LongIdent params. Both
/// scrutinees are their original names; the shared counter is consumed
/// by both (invisibly) and the matches nest left-outermost:
/// `match X with X -> match Y with Y -> 0`.
#[test]
fn diff_ast_fun_lambda_two_longident_args() {
    assert_asts_match("fun X Y -> 0\n");
}

/// Phase 5 — `fun (Some x) -> x`: a constructor-application pattern at
/// lambda-arg position. The parenthesised `LongIdentPat`-with-args is a
/// non-simple pat, so `SimplePatsOfPat` strips the single paren, sees the
/// `LongIdent("Some", [x])`, claims `_arg1`, and wraps the body:
/// `match _arg1 with Some x -> x`. Contrast the *nullary* longident arm
/// (`diff_ast_fun_lambda_longident_arg`), where the scrutinee keeps the
/// original name; a ctor-app head with args falls to FCS's generic
/// `_argN` catch-all instead. Pins that ctor-app-in-paren — which the
/// phase-4 function-form sweep already parses — lowers correctly at the
/// lambda-arg site.
#[test]
fn diff_ast_fun_lambda_ctor_app_arg() {
    assert_asts_match("fun (Some x) -> x\n");
}

/// Phase 5 Gap B — `fun (Some x) (Some y) -> x + y`: two *curried* ctor-app
/// paren args at lambda position. Each paren is a separate simple-pats
/// group claiming its own `_argN` slot. The single-checkpoint function-form
/// sweep used to over-reach past the first paren's swallowed `)` and fold
/// `(Some y)` into `Some`'s args; the swallowed-`)` raw-stream gate stops
/// the sweep so the two parens stay distinct curried parameters.
#[test]
fn diff_ast_fun_lambda_curried_ctor_app() {
    assert_asts_match("fun (Some x) (Some y) -> x + y\n");
}

/// Phase 5 Gap B — `fun (Some x) y -> x`: a ctor-app paren arg followed by a
/// bare named arg. The sweep must stop at the first paren's swallowed `)`,
/// leaving `y` as a second curried parameter (`SimplePat.Id y`) rather than
/// folding it into `Some`'s argument list.
#[test]
fn diff_ast_fun_lambda_ctor_then_named() {
    assert_asts_match("fun (Some x) y -> x\n");
}

/// Phase 5.X.6 — `fun (x : int) -> x`: a typed paren arg with a *simple*
/// inner pat. FCS's `SimplePatOfPat` recurses through `SynPat.Typed`
/// (`SyntaxTreeOps.fs:311-313`); the inner `Named x` is simple, so no
/// `_argN` slot is claimed and the body stays `Ident x` — the annotation
/// rides on the generated `SynSimplePat.Typed(Id x, int)` but, because the
/// inner is simple, no synthetic `match` is introduced. Pins the no-lowering
/// path for a typed lambda arg.
#[test]
fn diff_ast_fun_lambda_typed_simple_arg() {
    assert_asts_match("fun (x : int) -> x\n");
}

/// Phase 5.X.6 — `fun (Some x : int option) -> x`: a typed paren arg with a
/// *non-simple* inner (`Some x`, a ctor-app). `SimplePatOfPat` recurses into
/// the inner `LongIdent("Some", [x])`, which falls to the generic `_argN`
/// catch-all and produces the body lowering `match _arg1 with Some x -> x`.
/// The match clause matches the *inner* (un-annotated) pat — the `: int
/// option` survives only on the generated simple-pat — so the projected body
/// must agree with FCS exactly. Pins the lowered path for a typed lambda arg.
#[test]
fn diff_ast_fun_lambda_typed_ctor_arg() {
    assert_asts_match("fun (Some x : int option) -> x\n");
}

/// Phase 5.X.6 — `fun (_ : int) -> 0`: a typed paren arg whose inner is a
/// *wildcard*. `SimplePatOfPat` recurses to `SynPat.Wild`, which is the one
/// non-simple shape with `fn = None` (`SyntaxTreeOps.fs:354-355`): it claims
/// a (compiler-generated) `_argN` slot but introduces *no* synthetic match,
/// so the body stays `0`. Pins the slot-but-no-lowering wildcard path under a
/// typed wrapper.
#[test]
fn diff_ast_fun_lambda_typed_wildcard_arg() {
    assert_asts_match("fun (_ : int) -> 0\n");
}

/// Phase 5.X.6 — `fun (x : int) (y : string) -> x`: two *curried* typed paren
/// args, both simple-inner. Each typed paren is its own simple-pats group; no
/// lowering. Also a cross-check of the §5.X.4 Gap B swallowed-`)` sweep gate
/// under typed args — the first paren's `)` must end the first arg cleanly
/// rather than the per-element `:` or sweep reaching into the second paren.
#[test]
fn diff_ast_fun_lambda_curried_typed_args() {
    assert_asts_match("fun (x : int) (y : string) -> x\n");
}

/// Phase 5 — `fun (0, x) -> x`: a tuple parameter with a *non-simple*
/// element (the const `0`). Unlike the all-simple `fun (x, y) -> x`
/// (`diff_ast_fun_lambda_tuple_paren_arg`, which lowers to no match), a
/// non-simple element forces the whole `Paren(Tuple)` to take one
/// `_arg1` slot and match the original tuple pattern:
/// `match _arg1 with 0, x -> x`. Pins the tuple-of-non-simple lowering at
/// the lambda-arg site.
#[test]
fn diff_ast_fun_lambda_tuple_const_element_arg() {
    assert_asts_match("fun (0, x) -> x\n");
}

/// Phase 5 — `fun (Some x, y) -> x`: a tuple parameter mixing a
/// ctor-app element (`Some x`) with a simple one (`y`). Same mechanism as
/// `diff_ast_fun_lambda_tuple_const_element_arg`: the non-simple `Some x`
/// element makes the whole tuple non-simple, so it claims one `_arg1`
/// slot and matches the original pattern:
/// `match _arg1 with Some x, y -> x`.
#[test]
fn diff_ast_fun_lambda_tuple_ctor_app_element_arg() {
    assert_asts_match("fun (Some x, y) -> x\n");
}

/// Phase 6 (Gap B) — `fun (Some x) (Some y) -> x`: two *curried*
/// constructor-application params. FCS nests two `Lambda`s; the
/// `PushCurriedPatternsToExpr` foldBack consumes args right-to-left, so
/// the inner `(Some y)` claims `_arg1` and the outer `(Some x)` claims
/// `_arg2`, lowering to
/// `match _arg2 with Some x -> match _arg1 with Some y -> x`. The
/// body-lowering fold already handles this; what regressed was the
/// *parser* — the curried-arg sweep over-ran the enclosing `)` when a
/// paren arg held a `LongIdentPat`-with-args. Pins the fix.
#[test]
fn diff_ast_fun_lambda_curried_ctor_app_args() {
    assert_asts_match("fun (Some x) (Some y) -> x\n");
}

/// Phase 6 (Gap B) — `fun (Some x) y -> x`: a non-simple paren arg
/// *followed by* a simple one. Only `(Some x)` is non-simple, so it
/// claims `_arg1` while `y` stays a plain simple pat; the body lowers to
/// `match _arg1 with Some x -> x`. The mirror `fun x (Some y) -> y`
/// (simple first) already parsed before this fix — the bug needed a
/// non-simple *first* paren arg with another curried arg after it.
#[test]
fn diff_ast_fun_lambda_ctor_app_then_simple() {
    assert_asts_match("fun (Some x) y -> x\n");
}

/// Phase 6 (Gap B) — `fun (x) y -> x`: a plain *parenthesised identifier*
/// followed by a second curried arg. The paren holds only a `Named("x")`,
/// so FCS projects `(x)` to a simple named pat (no function-form). But
/// LexFilter swallows the `)`, so the function-form decision in
/// `try_emit_head_binding_pat_element` peeks past it and sees `y`,
/// mis-classifying `x` as a function-form head. The raw-`)` gate must
/// veto the function-form *decision*, not just the arg sweep.
#[test]
fn diff_ast_fun_lambda_paren_ident_then_simple() {
    assert_asts_match("fun (x) y -> x\n");
}

/// Phase 5.3 — `fun ((x, y)) -> x`: a tuple parameter wrapped in an
/// *extra* paren layer. FCS's `SimplePatsOfPat` only special-cases a
/// *single* `Paren(Tuple)` (`SyntaxTreeOps.fs:389`); the double paren
/// `Paren(Paren(Tuple))` misses that arm and falls through to
/// `SimplePatOfPat`, which consumes `_arg1` and wraps the body in
/// `match _arg1 with (x, y) -> x`. Contrast `fun (x, y) -> x`
/// (`diff_ast_fun_lambda_tuple_paren_arg`), which lowers to no match.
/// Pins that the lowering strips only the outermost paren before
/// classifying.
#[test]
fn diff_ast_fun_lambda_double_paren_tuple_arg() {
    assert_asts_match("fun ((x, y)) -> x\n");
}

/// Phase 5.3 — `fun (()) -> 1`: a unit parameter wrapped in an extra
/// paren. Same mechanism as `diff_ast_fun_lambda_double_paren_tuple_arg`:
/// FCS's `Paren(Const Unit)` special case (`SyntaxTreeOps.fs:397`) only
/// fires for a single paren, so `Paren(Paren(Const Unit))` lowers via
/// `SimplePatOfPat` to `match _arg1 with () -> 1` rather than the
/// empty-simple-pats no-match form of `fun () -> 1`.
#[test]
fn diff_ast_fun_lambda_double_paren_unit_arg() {
    assert_asts_match("fun (()) -> 1\n");
}

/// Phase 5 — `fun 0 -> fun 1 -> 2`: a non-simple lambda directly nesting
/// another. FCS's `SynArgNameGenerator` lives on the lexbuf and is
/// `.Reset()` only at each module-level definition (`pars.fsy:1310`), so
/// the counter is *shared across both lambdas*. The parser reduces the
/// inner lambda first (bottom-up), so the inner `1` claims `_arg1` and the
/// outer `0` claims `_arg2`: the lowered outer body is
/// `match _arg2 with 0 -> match _arg1 with 1 -> 2`. Pins that the
/// normaliser shares one counter across the whole definition rather than
/// resetting per `FUN_EXPR` (which would mis-number the outer as `_arg1`).
#[test]
fn diff_ast_fun_lambda_nested_const_args() {
    assert_asts_match("fun 0 -> fun 1 -> 2\n");
}

/// Phase 5 — `(fun 0 -> 1), (fun 2 -> 3)`: two sibling non-simple lambdas
/// in one tuple expression. The shared per-definition counter is consumed
/// in parse-reduction (left-to-right) order, so the left lambda's `0`
/// claims `_arg1` and the right lambda's `2` claims `_arg2`. Pins that the
/// counter threads across sibling expression subtrees, not just nested
/// ones.
#[test]
fn diff_ast_fun_lambda_sibling_const_lambdas() {
    assert_asts_match("(fun 0 -> 1), (fun 2 -> 3)\n");
}

/// Phase 5 — `let rec a = fun 0 -> 1 and b = fun 2 -> 3`: an `and`-chain
/// is a single `moduleDefn`, so FCS resets the counter once for the whole
/// set. `a`'s lambda claims `_arg1` and `b`'s claims `_arg2`. Pins that
/// the reset boundary is the whole `NormalisedDecl::Let` (all its
/// bindings), not each individual binding.
#[test]
fn diff_ast_fun_lambda_and_chain_shares_counter() {
    assert_asts_match("let rec a = fun 0 -> 1\nand b = fun 2 -> 3\n");
}

/// Phase 5 — two separate top-level bindings each containing a non-simple
/// lambda. Each is its own `moduleDefn`, so FCS `.Reset()`s the counter
/// between them and *both* lambdas claim `_arg1`. Pins that the per-decl
/// reset actually fires (a single shared counter that never reset would
/// give the second binding `_arg2`).
#[test]
fn diff_ast_fun_lambda_separate_bindings_reset_counter() {
    assert_asts_match("let a = fun 0 -> 1\nlet b = fun 2 -> 3\n");
}

/// Phase 5 Gap A — `(fun (x as y) -> y)`: FCS's `SimplePatOfPat`
/// (`SyntaxTreeOps.fs:340-341`) lowers `As(_, Named y)` to scrutinee
/// `Ident y` with **no** `_argN` slot, wrapping the body in
/// `match y with (x as y) -> y`. Pins that lowering against FCS.
#[test]
fn diff_ast_fun_as_pat_lowering() {
    assert_asts_match("(fun (x as y) -> y)\n");
}

/// Phase 5 Gap B — `(fun (x as y) 0 -> y)`: the `As(_, Named y)` paren arg
/// burns *no* `_argN` slot (scrutinee `Ident y`), while the trailing const
/// arg `0` is non-simple and claims `_arg1`. The function-form sweep used
/// to fold the `0` into the paren arg by over-reaching past the swallowed
/// `)`; the raw-stream gate keeps `0` as a separate curried parameter, so
/// FCS and we agree on the slot accounting.
#[test]
fn diff_ast_fun_as_pat_slot_interaction() {
    assert_asts_match("(fun (x as y) 0 -> y)\n");
}

/// Phase 5 Gap B — `(fun (x) 0 -> y)`: a simple named paren arg followed by
/// a non-simple const arg. The paren `x` lowers to scrutinee `Ident x` (no
/// slot, single-paren strip), the `0` claims `_arg1`. The sweep must stop
/// at the paren's swallowed `)` rather than promoting `(x)` to function
/// form and folding `0` into a bogus arg list.
#[test]
fn diff_ast_fun_paren_then_const_arg() {
    assert_asts_match("(fun (x) 0 -> y)\n");
}

/// Phase 6 — `fun [x] -> x`: `SynPat.ArrayOrList` is non-simple, so
/// `SimplePatOfPat` claims an `_arg1` slot and wraps the body in
/// `match _arg1 with [x] -> x`. Pins the generic-catch-all lowering.
#[test]
fn diff_ast_fun_list_pat_arg() {
    assert_asts_match("fun [x] -> x\n");
}

/// Phase 6.5 — a record pattern as a `fun` lambda argument: `fun { X = a } ->
/// a`. Non-simple, so FCS's `SimplePatsOfPat` lowers it to
/// `match _arg1 with { X = a } -> a`; the body-lowering fold must fire.
#[test]
fn diff_ast_fun_record_pat_arg() {
    assert_asts_match("fun { X = a } -> a\n");
}

/// Phase 6.6 — an IsInst pattern as a parenthesised `fun` lambda argument:
/// `fun (:? int) -> 0`. IsInst is non-simple, so FCS's `SimplePatsOfPat`
/// (`SyntaxTreeOps.fs:347`) lowers the body to `match _arg1 with :? int -> 0`
/// (the catch-all claims an `_argN` slot, strips the paren in the clause).
/// Pins that the body-lowering fold fires on IsInst.
#[test]
fn diff_ast_fun_isinst_pat_arg() {
    assert_asts_match("fun (:? int) -> 0\n");
}

/// Phase 6.7 — a cons pattern as a parenthesised `fun` lambda argument:
/// `fun (h :: t) -> h`. `ListCons` is non-simple, so FCS's `SimplePatsOfPat`
/// catch-all lowers the body to `match _arg1 with h :: t -> h`; pins the
/// body-lowering fold fires (no normaliser change needed).
#[test]
fn diff_ast_fun_cons_pat_arg() {
    assert_asts_match("fun (h :: t) -> h\n");
}

/// Phase 6.8 — a conjunction as a parenthesised `fun` lambda argument:
/// `fun (a & b) -> a`. `Ands` is non-simple, so FCS's `SimplePatsOfPat`
/// catch-all lowers the body to `match _arg1 with a & b -> a`; pins the
/// body-lowering fold fires (no normaliser change needed).
#[test]
fn diff_ast_fun_ands_pat_arg() {
    assert_asts_match("fun (a & b) -> a\n");
}

/// Phase 6.9 — an or-pattern as a parenthesised `fun` lambda argument:
/// `fun (A | B) -> 0`. `Or` is non-simple, so FCS's `SimplePatsOfPat` catch-all
/// lowers the body to `match _arg1 with A | B -> 0`; pins the body-lowering
/// fold fires (no normaliser change needed).
#[test]
fn diff_ast_fun_or_pat_arg() {
    assert_asts_match("fun (A | B) -> 0\n");
}

/// Phase 5.M.4 — `function A -> 1`: the minimal `function` (MatchLambda)
/// form. `Token::Function` is rewritten by LexFilter to `Virtual::Function`
/// (`OFUNCTION`) at the same span, opening a `SynExpr.MatchLambda` with a
/// single clause and no scrutinee. FCS keeps this distinct from
/// `fun`+`match`, so it must project to `NormalisedExpr::MatchLambda`.
#[test]
fn diff_ast_function_single_clause() {
    assert_asts_match("function A -> 1\n");
}

/// Phase 5.M.4 — `function A -> 1 | B -> 2`: two single-line clauses
/// separated by a bare `|`, reusing the shared `match`-clause loop. One
/// trailing `RightBlockEnd`+`End` closes the whole construct.
#[test]
fn diff_ast_function_two_clauses() {
    assert_asts_match("function A -> 1 | B -> 2\n");
}

/// Phase 5.M.4 — an optional *leading* bar before the first `function`
/// clause. FCS elides the leading-bar range, so both forms project
/// identically.
#[test]
fn diff_ast_function_leading_bar() {
    assert_asts_match("function | A -> 1 | B -> 2\n");
}

/// Phase 5.M.4 — a `when` guard on a `function` clause. Confirms the
/// reused clause machinery carries the guard into `SynMatchClause`'s
/// `whenExpr` for the MatchLambda case too.
#[test]
fn diff_ast_function_when_guard() {
    assert_asts_match("function A when y -> 1\n");
}

/// Phase 5.M.4 — `let f = function A -> 1 | B -> 2`: the MatchLambda as a
/// value-binding RHS. Pins that `parse_function_expr` drains exactly its
/// own trailing `RightBlockEnd`+`End`, leaving the enclosing let's
/// `BlockEnd`/`DeclEnd` for `parse_let_binding` — the same single-pair
/// drain `parse_match_expr` performs.
#[test]
fn diff_ast_function_as_let_rhs() {
    assert_asts_match("let f = function A -> 1 | B -> 2\n");
}

/// Phase 5.M.4 — offside multi-line `function` clauses under a `let`. Each
/// clause is closed by its own `RightBlockEnd` before the next `Bar`, then
/// a single final `End`; reuses the offside-clause handling proven for
/// `match`.
#[test]
fn diff_ast_function_offside_clauses() {
    assert_asts_match("let f =\n    function\n    | A -> 1\n    | B -> 2\n");
}

/// Phase 5.M.4 — a `fun` lambda in both the guard and the result of a
/// `function` clause. The shared `_argN` generator must number the guard's
/// lambda `_arg1` and the result's `_arg2`, so the normaliser must thread
/// the counter through the guard before the result — same ordering pin as
/// the `match` case, exercised through the MatchLambda projection.
#[test]
fn diff_ast_function_fun_lambda_ordering() {
    assert_asts_match("function A when (fun a -> a) -> (fun b -> b)\n");
}

/// Phase 5.M.4 — a *parenthesised* `function`: `(function A -> 1)`. The
/// paren-expr dispatch peeks the raw token past `(` via
/// `next_non_trivia_raw_after` and gates on `raw_starts_minus_expr`; that
/// raw is `Token::Function` (LexFilter's `Virtual::Function` relabel only
/// surfaces in the filtered stream), so `function` must be a raw
/// minus-expr starter alongside `match`/`fun`/`if`, or the parenthesised
/// MatchLambda is rejected before the `Virtual::Function` arm can fire.
#[test]
fn diff_ast_function_parenthesised() {
    assert_asts_match("(function A -> 1)\n");
}

/// Phase 5.M.4 — a parenthesised `function` in argument position:
/// `List.map (function A -> 1)`. The common real-world shape; same raw
/// paren lookahead as `diff_ast_function_parenthesised`.
#[test]
fn diff_ast_function_paren_arg() {
    assert_asts_match("List.map (function A -> 1)\n");
}

/// Phase 5.M.5 — a sequential body in a `function` (MatchLambda) clause.
/// `parse_function_expr` shares `parse_match_clauses`, so it inherits the
/// sequential-body handling; this pins it through the MatchLambda projection.
#[test]
fn diff_ast_function_seq_body() {
    assert_asts_match("function\n| A ->\n    e1\n    e2\n");
}

/// Stage 2 — explicit `;` sequential in a `fun` body: `fun x -> printfn "a"; x`.
/// FCS parses the lambda body as `SynExpr.Sequential(App(printfn, "a"),
/// Ident x)`. The `;` is a raw `Token::Semi` separator, not an offside
/// `Virtual::BlockSep`; the shared seq-block gatherer must accept both.
#[test]
fn diff_ast_fun_lambda_semi_seq_body() {
    assert_asts_match("fun x -> printfn \"a\"; x\n");
}

// ============================================================================
// Phase 10.6 — parameter attributes (`SynPat.Attrib`)
// ============================================================================
//
// FCS reaches `SynPat.Attrib(pat, attributes, range)` (`SyntaxTree.fsi:1116`)
// only via `attributes parenPattern` (`pars.fsy:3940`), so an attribute list
// prefixes a *parenthesised* (or list/array) pattern element. `SimplePatOfPat`
// recurses through `Attrib` exactly like `Typed` (`SyntaxTreeOps.fs:315`), so
// the `fun`-body lowering decision is taken by the *inner* pattern: a simple
// inner (`[<Foo>] x`) scaffolds no `match`; a non-simple inner
// (`[<Foo>] Some x`) does, with the inner pat — *not* the `Attrib` — in the
// synthesised clause.

/// Lambda arg `fun ([<Foo>] x) -> x`. The arg projects to
/// `Paren(Attrib(Named x, [[Foo]]))`; the inner `x` is simple, so the body
/// stays a bare `Ident x` (no `match` scaffold).
#[test]
fn diff_ast_fun_param_attrib() {
    assert_asts_match("fun ([<Foo>] x) -> x\n");
}

/// Let function-form head `let f ([<Foo>] x) = x`. The attribute rides on the
/// head's argument pattern (`LongIdent f [Paren(Attrib(Named x))]`), distinct
/// from a binding-level attribute carrier (which would be a leading
/// `[<…>] let`).
#[test]
fn diff_ast_let_head_param_attrib() {
    assert_asts_match("let f ([<Foo>] x) = x\n");
}

/// Non-simple inner `fun ([<Foo>] Some x) -> x`. The inner `Some x` is a
/// constructor application, so the lowering scaffolds
/// `match _arg1 with Some x -> x` — the `Attrib` is dropped from the clause
/// pattern (it survives only on the elided `SynSimplePat.Attrib`), pinning
/// that the lowering follows the inner pat.
#[test]
fn diff_ast_fun_param_attrib_non_simple() {
    assert_asts_match("fun ([<Foo>] Some x) -> x\n");
}

/// Attribute arguments on a pattern are expression subtrees in source order.
/// The lambda inside `[<Foo(…)>]` must consume the same shared `_argN`
/// generator as the surrounding lambda body, rather than a local pattern-only
/// counter. The generated `_arg<N>` index is canonicalised by the diff, but this
/// pins that the argument expression is projected through the shared path.
#[test]
fn diff_ast_fun_param_attrib_arg_lambda() {
    assert_asts_match("fun ([<Foo(fun 0 -> 0)>] Some x) -> fun 1 -> x\n");
}

/// Precedence: `:` binds *inside* the attrib — `([<Foo>] x : int)` is
/// `Attrib(Typed(Named x, int))`, not `Typed(Attrib(Named x), int)`.
#[test]
fn diff_ast_fun_param_attrib_colon_inside() {
    assert_asts_match("fun ([<Foo>] x : int) -> x\n");
}

/// Precedence: `::` binds *inside* the attrib — `([<Foo>] h :: t)` is
/// `Attrib(ListCons(h, t))`.
#[test]
fn diff_ast_fun_param_attrib_cons_inside() {
    assert_asts_match("fun ([<Foo>] h :: t) -> h\n");
}

/// Precedence: `&` binds *inside* the attrib — `([<Foo>] x & y)` is
/// `Attrib(Ands[x, y])`.
#[test]
fn diff_ast_fun_param_attrib_amp_inside() {
    assert_asts_match("fun ([<Foo>] x & y) -> x\n");
}

/// Precedence: `,` binds *outside* the attrib — `([<Foo>] x, y)` is
/// `Tuple[Attrib(x), y]`, so the attribute prefixes only the first element.
#[test]
fn diff_ast_fun_param_attrib_comma_outside() {
    assert_asts_match("fun ([<Foo>] x, y) -> x\n");
}

/// Precedence: `as` binds *outside* the attrib — `([<Foo>] x as y)` is
/// `As(Attrib(x), y)`.
#[test]
fn diff_ast_fun_param_attrib_as_outside() {
    assert_asts_match("fun ([<Foo>] x as y) -> x\n");
}

/// Precedence: `|` binds *outside* the attrib — `([<Foo>] A | B)` is
/// `Or(Attrib(A), B)`.
#[test]
fn diff_ast_fun_param_attrib_or_outside() {
    assert_asts_match("fun ([<Foo>] A | B) -> A\n");
}

/// A continuation-position attribute `(y, [<Foo>] x)` → `Tuple[y, Attrib(x)]`,
/// pinning that the `,`-continuation element also accepts the attribute prefix
/// (the `emit_pat_atom` gate admits `[<` inside parens).
#[test]
fn diff_ast_fun_param_attrib_tuple_second() {
    assert_asts_match("fun (y, [<Foo>] x) -> x\n");
}

/// Two *adjacent* lists `([<A>] [<B>] x)` group into a single `Attrib` carrying
/// both `SynAttributeList`s (FCS's `attributes: attributeList attributes`).
#[test]
fn diff_ast_fun_param_attrib_two_lists() {
    assert_asts_match("fun ([<A>] [<B>] x) -> x\n");
}

/// One list, two `;`-separated attributes `([<A; B>] x)` → a single
/// `SynAttributeList` with two `SynAttribute`s (composes with 10.5a).
#[test]
fn diff_ast_fun_param_attrib_semicolon_list() {
    assert_asts_match("fun ([<A; B>] x) -> x\n");
}

/// A list-*pattern* element may be attributed: `fun [ x; [<Foo>] y ] -> y` →
/// `ArrayOrList(false, [Named x, Attrib(Named y)])`, reached through the same
/// `emit_paren_pat_element` hook the paren form uses.
#[test]
fn diff_ast_list_pat_attrib_element() {
    assert_asts_match("fun [ x; [<Foo>] y ] -> y\n");
}

/// The attributed pattern may sit on a fresh offside line after the list —
/// FCS's `attributeList` `opt_OBLOCKSEP` (`fun ([<A>]⏎    x) -> x` →
/// `Paren(Attrib(Named x, [[A]]))`). The `Virtual::BlockSep` LexFilter parks
/// between `>]` and `x` must be drained before the inner pattern.
#[test]
fn diff_ast_fun_param_attrib_multiline() {
    assert_asts_match("fun ([<A>]\n    x) -> x\n");
}

/// A type annotation after a lambda body binds the *body*, not the whole
/// lambda — FCS's `typedSequentialExpr: sequentialExpr COLON typ` makes the
/// lambda body (`typedSequentialExprBlockR`) a full `typedSequentialExpr`. So
/// `fun x -> y : int` is `Lambda(body = Typed(y, int))`, not
/// `Typed(Lambda(x, y), int)`. (Regression: corpus `TrieMappingTests.fs`, a
/// record-returning lambda `(fun … -> { … } : T)` as an `Array.mapi` arg.)
#[test]
fn diff_ast_fun_lambda_body_type_annotation() {
    // As an application argument (the corpus shape).
    assert_asts_match("g (fun x -> y : int)\n");
    // As a parenthesised expression.
    assert_asts_match("let f = (fun x -> y : int)\n");
    // Record-returning body, like the corpus.
    assert_asts_match("g (fun x -> { A = x } : T)\n");
    // Multi-statement lambda body: the `: T` binds the whole sequence.
    assert_asts_match("g (fun x ->\n    h x\n    y : int)\n");
}

/// An *offside* annotation after a lambda binds the whole lambda (the outer
/// `typedSequentialExpr`), not the body: `let f =⏎    fun x ->⏎        x⏎    :
/// int` is `Typed(Lambda, int)`. The annotation colon must be the *real* next
/// filtered token — not a pending lambda close-virtual (`ORIGHT_BLOCK_END` /
/// `OEND`) that the raw stream skips past.
#[test]
fn diff_ast_offside_annotation_after_lambda() {
    assert_asts_match("let f =\n    fun x ->\n        x\n    : int\n");
}

/// A lambda body's trailing type annotation whose type is a bare `appType`
/// head (path or typar), followed by a fresh expression statement on the next
/// line. The `ORIGHT_BLOCK_END` that closes the `->` one-sided block parks at
/// the filtered cursor right after the annotation type, while the raw stream
/// skips past it to the next line's ident — so `parse_app_type`'s postfix-app
/// loop must not treat that ident as a postfix head of the annotation type.
/// (Regression: corpus `neg117.fs` / `neg120.fs` / `pos34.fs` /
/// `ILGenCodegen/CastThenBr.fs`, all of which tripped a `parse_app_type_con_power`
/// `unreachable!`.)
#[test]
fn diff_ast_lambda_body_annotation_then_expr_statement() {
    // Path-typed annotation (`int`), bare-ident continuation.
    assert_asts_match("let f = fun x -> x : int\ny\n");
    // Typar-typed annotation (`'r`).
    assert_asts_match("let f = fun x -> x : 'r\ny\n");
    // Same shape on a `match` arm body (the arrow that closes is the arm `->`).
    assert_asts_match("let g a = match a with _ -> a : int\ny\n");
}

/// An optional-value lambda parameter — `fun ?x -> x`. FCS's `SimplePatOfPat`
/// maps `SynPat.OptionalVal` directly to a simple optional `SynSimplePat.Id`
/// with *no* generated `match` (`SyntaxTreeOps.fs:321`), exactly as a `Named`
/// arg lowers to a direct parameter. So the lambda body stays the bare `x`, not
/// a `match _arg1 with ?x -> x`. (Optional args are only semantically valid on
/// type members, but parsing and the simple-pat lowering are identical.)
#[test]
fn diff_ast_fun_lambda_optional_val_arg() {
    assert_asts_match("fun ?x -> x\n");
}

/// Two consecutive type definitions whose members each lower a non-simple `fun`
/// parameter (`fun (KeyValue (k, v)) -> …`). FCS's `SynArgNameGenerator` is
/// reset per top-level `let`/`use` but **carried across `type` definitions and
/// their members**, so the second type's synthesised scrutinee is `_arg2`, not
/// `_arg1`. The diff canonicalises the `_arg<N>` index on both sides (see
/// `canonicalise_synth_arg`) so the structural lowering still matches without
/// the projector replicating FCS's stateful counter discipline. Regression for
/// the corpus divergences in `SomethingToCompile.fs` / `FSharpWorkspaceState.fs`.
#[test]
fn diff_ast_fun_arg_counter_carries_across_types() {
    assert_asts_match(
        "type T() =\n    member _.F = fun (KeyValue (k, v)) -> k, v\ntype U() =\n    member _.G = fun (KeyValue (a, b)) -> b, a\n",
    );
}

/// A source identifier literally spelled `_arg<N>` on the named side of an `as`
/// pattern (`fun (x as _arg1) -> _arg1`). FCS's `As(_, Named id)` special-case
/// scrutinises the *user* name, so the scrutinee is `Ident "_arg1"`. The diff
/// canonicalises `_arg<N>` on both sides (`canonicalise_synth_arg`), so the
/// user-written name collapses identically and the lowering still matches.
#[test]
fn diff_ast_fun_as_pattern_user_arg_name() {
    assert_asts_match("let f = fun (x as _arg1) -> _arg1\n");
}

/// Distinct *source* identifiers named `_arg<N>` used as ordinary value
/// references (`let f _arg1 _arg2 = _arg1, _arg2`) must stay distinct: the
/// `_arg<N>` canonicalisation is scoped to `match` *scrutinees*
/// (`canonicalise_scrutinee`), so general value references are untouched and a
/// projector regression that mis-ordered them would still be caught.
#[test]
fn diff_ast_user_arg_named_value_refs_stay_distinct() {
    assert_asts_match("let f _arg1 _arg2 = _arg1, _arg2\n");
}

// ---- Phase 11 error recovery: incomplete lambda body ---------------------
//
// `fun … ->` with nothing after `->` — a common mid-edit state. FCS recovers
// the body as `SynExpr.ArbitraryAfterError`, which projects to the shared
// `NormalisedExpr::Error` marker; our parser leaves the body an empty `ERROR`,
// which `normalise_fun` projects the same way (then folds the args around it,
// exactly as for a real body — so the `_argN` lowering stays symmetric). The
// trailing `let y = 2` survives as its own decl (the offside rule closes the
// lambda).

/// `fun a ->` with a missing body — `Lambda([a], Error)`.
#[test]
fn diff_ast_fun_lambda_recover_missing_body() {
    assert_asts_match_allow_errors("let x = fun a ->\nlet y = 2\n");
}

/// A tuple parameter with a missing body — `fun (p, q) ->`. The non-simple
/// parameter triggers the `_argN` match-lowering, which must wrap the recovered
/// `Error` body identically to FCS's `ArbitraryAfterError`.
#[test]
fn diff_ast_fun_lambda_recover_missing_body_tuple_arg() {
    assert_asts_match_allow_errors("let x = fun (p, q) ->\nlet y = 2\n");
}

/// A nested lambda with the inner body missing — `fun a -> fun b ->`. Exercises
/// the shared `_argN` counter ordering with the recovery hole as the innermost
/// body.
#[test]
fn diff_ast_fun_lambda_recover_missing_body_nested() {
    assert_asts_match_allow_errors("let x = fun a -> fun b ->\nlet y = 2\n");
}
