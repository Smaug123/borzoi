//! Differential test (`parser::parse` vs FCS): `let` declarations — value and
//! function form, `rec`/`and` chains, `inline`/`mutable`, and binding-position
//! patterns. Split out of the former monolithic `parser_diff.rs`.

use crate::common::{assert_asts_match, assert_asts_match_allow_errors};

/// Phase 4.1 — top-level `let x = 1`. FCS produces
/// `SynModuleDecl.Let(isRec=false, [SynBinding(_, Normal, false, false, …,
/// SynPat.Named(SynIdent("x", _), false, None, _), …, SynExpr.Const(Int32 1),
/// …)], _, _)`. Our parser projects to the same `NormalisedDecl::Let` shape
/// with a single `NormalisedPat::Named("x")` binding whose RHS is
/// `NormalisedExpr::Const(Int32(1))`. This is the canonical Phase 4.1 path.
#[test]
fn diff_ast_let_x_eq_int() {
    assert_asts_match("let x = 1\n");
}

/// Phase 4.1 — RHS is a one-segment ident. Confirms the binding's RHS
/// goes through the same `parse_expr` machinery as a top-level
/// expression decl (so an `Ident` atom on the RHS projects to
/// `NormalisedExpr::Ident`, not `LongIdent`).
#[test]
fn diff_ast_let_x_eq_ident() {
    assert_asts_match("let x = y\n");
}

/// Phase 4.1 — RHS is an infix application `1 + 2`. The Pratt climber
/// must continue past the binding's `=` so the RHS isn't truncated at
/// the first atom. FCS shape: `App(App(op_Addition, 1), 2)` with
/// `isInfix=true` on the inner App.
#[test]
fn diff_ast_let_x_eq_infix() {
    assert_asts_match("let x = 1 + 2\n");
}

/// Phase 4.1 — RHS is a parenthesised expression. Confirms the
/// `Virtual::BlockBegin` between `=` and the RHS doesn't interfere with
/// `parse_paren_expr`'s nested call to `parse_expr`.
#[test]
fn diff_ast_let_x_eq_paren() {
    assert_asts_match("let x = (1)\n");
}

/// Phase 4.2 — `let rec` projects to `SynModuleDecl.Let(isRec = true, …)`.
/// Confirms `REC_TOK` is plumbed all the way through `LetDecl::is_rec` to
/// the normaliser without any other shape divergence.
#[test]
fn diff_ast_let_rec_single() {
    assert_asts_match("let rec f = 1\n");
}

/// Phase 4.2 — `and`-chain without `rec`. FCS rejects this with error
/// FS0576 (`parsLetAndForNonRecBindings`, pars.fsy:3073) at the `let`
/// keyword but still emits a `SynModuleDecl.Let(isRec = false, [f; g])`.
/// We mirror the same shape *and* the same diagnostic; the assertion only
/// compares AST projections (the diagnostic message text differs from
/// FCS's).
#[test]
fn diff_ast_let_and_chain() {
    assert_asts_match_allow_errors("let f = 1\nand g = 2\n");
}

/// Phase 4.2 — the combined form. Single `Let` with `isRec = true` and two
/// bindings.
#[test]
fn diff_ast_let_rec_and_chain() {
    assert_asts_match("let rec f = 1\nand g = 2\n");
}

/// Phase 4.2 — three-binding chain. Sanity check that the chain extends
/// beyond two without producing extra decls.
#[test]
fn diff_ast_let_and_chain_three() {
    assert_asts_match("let rec f = 1\nand g = 2\nand h = 3\n");
}

/// Phase 4.3 — `let mutable x = 1` projects to `SynBinding(isMutable = true,
/// isInline = false)`. Confirms `MUTABLE_TOK` is plumbed all the way through
/// `Binding::is_mutable` to the normaliser without other shape divergence.
#[test]
fn diff_ast_let_mutable() {
    assert_asts_match("let mutable x = 1\n");
}

/// Phase 4.3 — `let inline f = 1` projects to `SynBinding(isMutable = false,
/// isInline = true)`. Mirror of [`diff_ast_let_mutable`] for the
/// `INLINE_TOK` plumbing.
#[test]
fn diff_ast_let_inline() {
    assert_asts_match("let inline f = 1\n");
}

/// Phase 4.3 — `let inline mutable x = 1`: FCS's canonical
/// `opt_inline opt_mutable` order. Both flags `true` on the same binding.
#[test]
fn diff_ast_let_inline_mutable() {
    assert_asts_match("let inline mutable x = 1\n");
}

/// Phase 4.3 — `let rec inline f = 1`: `rec` at LET_DECL level coexisting
/// with `inline` at BINDING level. Confirms the two modifier sources stay
/// independent.
#[test]
fn diff_ast_let_rec_inline() {
    assert_asts_match("let rec inline f = 1\n");
}

/// Phase 4.3 — `let rec f = 1\nand mutable g = 2\n`: per-binding placement
/// of modifiers inside an `and`-chain. The two bindings have different
/// flags (`mutable` only on the second).
#[test]
fn diff_ast_let_and_chain_per_binding_mutable() {
    assert_asts_match("let rec f = 1\nand mutable g = 2\n");
}

/// Phase 4.4 — `let f x = 1`: function-form binding projects to
/// `SynPat.LongIdent(longDotId = ["f"], extraId = None, typars = None,
/// args = SynArgPats.Pats [SynPat.Named "x"], accessibility = None, _)`.
/// Confirms the parser's function-form branch lines up with FCS's shape.
#[test]
fn diff_ast_let_function_form_single_arg() {
    assert_asts_match("let f x = 1\n");
}

/// Phase 4.4 — `let f x y z = 1`: three curried args. Confirms the arg
/// sweep collects every trailing ident in source order.
#[test]
fn diff_ast_let_function_form_three_args() {
    assert_asts_match("let f x y z = 1\n");
}

/// An *adjacent* parenthesised argument `let f(x) = x`. LexFilter inserts a
/// `HighPrecedenceParenApp` virtual between the function name and the `(`; the
/// arg sweep skips it so the paren pattern parses, projecting to
/// `SynPat.LongIdent(["f"], args = Pats[Paren(Named "x")])` — the same shape as
/// the spaced `let f (x) = x`.
#[test]
fn diff_ast_let_adjacent_paren_arg() {
    assert_asts_match("let f(x) = x\n");
}

/// An adjacent *unit* argument `let f() = 1` → `Pats[Paren(Const Unit)]`.
#[test]
fn diff_ast_let_adjacent_unit_arg() {
    assert_asts_match("let f() = 1\n");
}

/// Multiple adjacent paren args `let f(x)(y) = x` → two `Paren` args, each
/// preceded by its own `HighPrecedenceParenApp`.
#[test]
fn diff_ast_let_adjacent_paren_args_curried() {
    assert_asts_match("let f(x)(y) = x\n");
}

/// A mix of adjacent-paren and spaced args `let f(x) y = x` →
/// `Pats[Paren(Named "x"), Named "y"]`.
#[test]
fn diff_ast_let_adjacent_paren_then_ident_arg() {
    assert_asts_match("let f(x) y = x\n");
}

/// An adjacent paren *after a non-paren arg* (`let f x(y) = x`) is two
/// successive patterns: FCS reports "Successive patterns should be separated by
/// spaces or tupled" and recovers to `Pats[Named "x", Paren(Named "y")]`. The
/// parser mirrors the diagnostic (only an HPA applied to the head name or a
/// paren arg is silent) and recovers to the same shape, so the `allow_errors`
/// diff lines up. (Contrast `diff_ast_let_adjacent_paren_args_curried`: a paren
/// *after a paren* — `f(x)(y)` — is a valid curried application, no error.)
#[test]
fn diff_ast_let_successive_patterns_is_error() {
    assert_asts_match_allow_errors("let f x(y) = x\n");
}

/// Phase 4.4 — `let inline f x = x`: function-form combined with
/// `inline` (a per-binding modifier). The Rust parser must consume the
/// modifier *then* take the function-form pat branch, matching FCS's
/// `SynBinding(isInline = true, headPat = SynPat.LongIdent …)`.
#[test]
fn diff_ast_let_inline_function_form() {
    assert_asts_match("let inline f x = x\n");
}

/// Phase 4.4 — `let rec f x = f x`: function-form combined with `rec`
/// (a per-`let` modifier). The recursive call on the RHS reads the
/// just-bound `f`. Verifies `is_rec = true` and the LONG_IDENT_PAT
/// shape coexist.
#[test]
fn diff_ast_let_rec_function_form() {
    assert_asts_match("let rec f x = f x\n");
}

/// Phase 4.5 — `let _ = 1`: wildcard head projects to `SynPat.Wild`.
/// Confirms the parser emits `WILDCARD_PAT` (no function-form promotion)
/// and the normaliser projects to a shared `NormalisedPat::Wildcard`.
#[test]
fn diff_ast_let_wildcard_head() {
    assert_asts_match("let _ = 1\n");
}

/// Phase 4.5 — `let f _ = 1`: function-form binding with a wildcard
/// arg. FCS projects the arg to `SynPat.Wild` inside
/// `SynArgPats.Pats[…]`; verifies the Rust-side arg-sweep loop and
/// normaliser cover the mixed-arg shape.
#[test]
fn diff_ast_let_function_form_wildcard_arg() {
    assert_asts_match("let f _ = 1\n");
}

/// Phase 4.5 — `let f x _ y = 1`: function-form with a sequence of
/// named and wildcard curried args. Verifies source order is preserved
/// for both sides.
#[test]
fn diff_ast_let_function_form_mixed_args() {
    assert_asts_match("let f x _ y = 1\n");
}

/// Phase 6.1 — `let () = ()`: unit-literal head projects to
/// `SynPat.Const(SynConst.Unit, _)`. FCS rejects this semantically (no
/// value to bind) but parses cleanly; we mirror the shape. Smallest
/// possible probe of the new `CONST_PAT > [LPAREN_TOK, RPAREN_TOK]`
/// surface.
#[test]
fn diff_ast_let_unit_value_head() {
    assert_asts_match("let () = ()\n");
}

/// Phase 6.1 — `let (x) = 1`: parenthesised value-form head. FCS keeps
/// `SynPat.Paren` in the AST (`SyntaxTree.fsi:1143`); the normaliser
/// must preserve the wrapping on both sides.
#[test]
fn diff_ast_let_paren_value_head() {
    assert_asts_match("let (x) = 1\n");
}

/// Phase 6.1 — `let null = 1`: null-pattern head projects to
/// `SynPat.Null`. FCS rejects semantically; we mirror the parse.
#[test]
fn diff_ast_let_null_value_head() {
    assert_asts_match("let null = 1\n");
}

/// Phase 6.1 — `let 0 = 1`: integer-literal head projects to
/// `SynPat.Const(SynConst.Int32 0, _)`. Locks in the const-pat surface
/// for numeric literals.
#[test]
fn diff_ast_let_int_lit_value_head() {
    assert_asts_match("let 0 = 1\n");
}

/// Phase 6.1 — `let true = 1`: bool-literal head projects to
/// `SynPat.Const(SynConst.Bool true, _)`.
#[test]
fn diff_ast_let_bool_lit_value_head() {
    assert_asts_match("let true = 1\n");
}

/// Phase 6.1 — `let f (x) = 1`: function-form with a parenthesised
/// curried arg. Exercises the arg-sweep loop calling `parse_atomic_pat`.
#[test]
fn diff_ast_let_function_form_paren_arg() {
    assert_asts_match("let f (x) = 1\n");
}

/// Phase 6 — `let f (Some x) = x`: function-form with a *constructor-
/// application* curried arg. FCS projects the binding head to
/// `LongIdent("f", Pats[Paren(LongIdent("Some", [Named x]))])`. The
/// phase-4 function-form sweep (`try_emit_head_binding_pat_element`)
/// already parses the ctor-app inside the paren. Pins the single-arg
/// case at the let-head site; the multi-arg curried form
/// `let f (Some x) (Some y) = x` is covered by the Gap-B sweep fix and
/// pinned by `diff_ast_let_function_form_curried_ctor_app`.
#[test]
fn diff_ast_let_function_form_ctor_app_arg() {
    assert_asts_match("let f (Some x) = x\n");
}

/// Phase 5 Gap B — `let f (Some x) (Some y) = x`: function-form with two
/// *curried* ctor-app paren args. FCS projects
/// `LongIdent("f", Pats[Paren(LongIdent Some [x]); Paren(LongIdent Some [y])])`.
/// The single-checkpoint sweep previously over-reached past the first
/// paren's swallowed `)` (the sweep peeked the filtered stream, where the
/// `)` is gone, and folded `(Some y)` into `Some`'s args). The swallowed-`)`
/// raw-stream gate stops the sweep at the `)`, so each paren arg is a
/// separate curried parameter.
#[test]
fn diff_ast_let_function_form_curried_ctor_app() {
    assert_asts_match("let f (Some x) (Some y) = x\n");
}

/// Phase 6 (Gap B) — `let f (x) (y) = x`: a genuine function-form head
/// `f` whose curried args are each a plain parenthesised ident. FCS
/// projects `LongIdent("f", Pats[Paren(Named x); Paren(Named y)])` — the
/// inner idents stay simple named pats. Guards the decision gate against
/// over-vetoing: the swallowed-`)` check must reject only the
/// *paren-content* head, never the real `f` head whose args follow.
#[test]
fn diff_ast_let_function_form_curried_paren_idents() {
    assert_asts_match("let f (x) (y) = x\n");
}

/// Phase 6 (Gap B) — `let f (Some x) y = x`: a non-simple *first* paren
/// arg (`Some x`) followed by a simple curried arg (`y`). FCS projects
/// `LongIdent("f", Pats[Paren(Some x); Named y])`. The curried-arg sweep
/// inside the first paren must stop at the swallowed `)` so the trailing
/// `y` becomes a sibling arg of `f`, not an extra arg of `Some`.
#[test]
fn diff_ast_let_function_form_ctor_app_then_simple() {
    assert_asts_match("let f (Some x) y = x\n");
}

/// Phase 6.1 — `let f () = 1`: function-form with a unit arg. FCS
/// projects to `SynArgPats.Pats[SynPat.Const(SynConst.Unit, _)]`.
#[test]
fn diff_ast_let_function_form_unit_arg() {
    assert_asts_match("let f () = 1\n");
}

/// Phase 6.2 — `let (x : int) = 1`: typed-pattern surface. FCS's
/// `parenPattern COLON typeWithTypeConstraints` rule (`pars.fsy:3929`)
/// emits `SynPat.Typed(pat, targetType, _)`, which is only reachable
/// *inside* `parenPattern` — so the top-level shape is
/// `SynPat.Paren(SynPat.Typed(SynPat.Named "x", SynType.LongIdent ["int"]), _)`.
/// We mirror that with
/// `PAREN_PAT > [LPAREN_TOK, TYPED_PAT > [NAMED_PAT, COLON_TOK,
/// LONG_IDENT_TYPE], RPAREN_TOK]`. Pins the smallest typed-pattern
/// shape — the entry point for all of phase 6.2.
#[test]
fn diff_ast_let_typed_named_value_head() {
    assert_asts_match("let (x : int) = 1\n");
}

/// Phase 6.2 — `let (_ : int) = 1`: typed-wildcard variant. The annotated
/// pattern is `SynPat.Wild`, not `SynPat.Named`; the wrapping
/// `SynPat.Paren(SynPat.Typed(_, _, _), _)` is otherwise identical.
#[test]
fn diff_ast_let_typed_wildcard_value_head() {
    assert_asts_match("let (_ : int) = 1\n");
}

/// Phase 6.2 — `let (x : 'a) = x`: typed-pat with a type-parameter
/// annotation. The annotation is `SynType.Var(SynTypar("a", _, _), _)`
/// rather than a `LongIdent`, exercising a different `SynType` branch
/// under the same `TYPED_PAT` surface.
#[test]
fn diff_ast_let_typed_typar_value_head() {
    assert_asts_match("let (x : 'a) = x\n");
}

/// Phase 6.2 — `let (x : int -> int) = id`: typed-pat with a function
/// type. FCS produces `SynType.Fun(LongIdent ["int"], LongIdent ["int"],
/// _, _)` inside the `SynPat.Typed`. Pins the right-associative
/// `FUN_TYPE` layer under `TYPED_PAT`.
#[test]
fn diff_ast_let_typed_arrow_value_head() {
    assert_asts_match("let (x : int -> int) = id\n");
}

/// Phase 6.2 — `let (x : int * string) = 1, "a"`: typed-pat with a
/// tuple type annotation. FCS emits `SynType.Tuple` with two
/// `SynTupleTypeSegment.Type` cells separated by a `Star` segment;
/// confirms `TUPLE_TYPE` lives correctly under `TYPED_PAT`.
#[test]
fn diff_ast_let_typed_tuple_value_head() {
    assert_asts_match("let (x : int * string) = 1, \"a\"\n");
}

/// Phase 6.2 — `let f (x : int) = x`: typed-pat as a function-form arg.
/// FCS projects to `SynPat.LongIdent("f", [Paren(Typed(Named "x",
/// LongIdent ["int"], _), _)])`. Confirms the typed-pat hook fires
/// inside the `parse_atomic_pat` arg-sweep loop, not just the value
/// head.
#[test]
fn diff_ast_let_function_form_typed_arg() {
    assert_asts_match("let f (x : int) = x\n");
}

/// Phase 6.3 — `let x, y = 1, 2`: minimal tuple-pattern surface. FCS
/// produces `SynPat.Tuple(false, [Named "x"; Named "y"], _, _)` at the
/// binding head (`SyntaxTree.fsi:1130`-ish, `pars.fsy` `headBindingPat`).
/// Mirrored as `TUPLE_PAT > [NAMED_PAT, COMMA_TOK, NAMED_PAT]`, flat —
/// not nested pairs, paralleling `TUPLE_TYPE` in phase 7.4. Pins the
/// smallest tuple-pat shape; entry point for all of phase 6.3.
#[test]
fn diff_ast_let_tuple_value_head() {
    assert_asts_match("let x, y = 1, 2\n");
}

/// Phase 6.3 — `let x, y, z = 1, 2, 3`: ternary tuple head, pinning
/// the flat-list shape. FCS's `SynPat.Tuple` is a flat list of
/// elementPats (not nested pairs), and our `TUPLE_PAT` mirrors that
/// — `[NAMED_PAT, COMMA_TOK, NAMED_PAT, COMMA_TOK, NAMED_PAT]`. No
/// element should itself be a tuple.
#[test]
fn diff_ast_let_ternary_tuple_value_head() {
    assert_asts_match("let x, y, z = 1, 2, 3\n");
}

/// Phase 6.3 — `let (x, y) = 1, 2`: paren-wrapped tuple at the head.
/// FCS produces `SynPat.Paren(SynPat.Tuple([Named "x"; Named "y"], …),
/// …)`. The paren wrap must survive (it's load-bearing in FCS — folded
/// away would be a phantom diff against the FCS AST) and the inner
/// shape is the same flat `TUPLE_PAT`.
#[test]
fn diff_ast_let_paren_tuple_head() {
    assert_asts_match("let (x, y) = 1, 2\n");
}

/// Phase 6.3 — `let (x, y : int) = 1, 2`: typed-pat per tuple element
/// inside parens. FCS's `tuplePat → patternAndTypeOrThisExpr (','
/// patternAndTypeOrThisExpr)+` (`pars.fsy:3929`) attaches the colon to
/// the immediately preceding tuple element, producing
/// `SynPat.Paren(SynPat.Tuple([Named "x"; Typed(Named "y", int, _)],
/// …), …)` — *not* `Paren(Typed(Tuple([…]), int, _))`. The typed wrap
/// binds tighter than the tuple comma here. Pins our per-element
/// `emit_atomic_or_typed_pat` layering inside `parse_paren_pat`.
#[test]
fn diff_ast_let_typed_tuple_inside_parens() {
    assert_asts_match("let (x, y : int) = 1, 2\n");
}

/// Phase 6.3 — multiline parenthesised tuple pattern. LexFilter
/// inserts a `Virtual::BlockSep` between the comma and the indented
/// next element when the tuple spans lines; the in-paren tuple loop
/// must step over it before deciding whether an element follows,
/// mirroring `parse_expr`'s tuple loop (~line 1175). Without the
/// drain the second element looks missing and we emit a spurious
/// "expected pattern after `,`".
#[test]
fn diff_ast_let_paren_tuple_multiline() {
    assert_asts_match("let (x,\n     y) = 1, 2\n");
}

/// Phase 6.3 — multiline top-level tuple-pattern head. Same
/// `Virtual::BlockSep` discipline as the paren case, but on the
/// top-level `maybe_wrap_tuple_pat` path. FCS keeps the
/// `CtxtLetDecl` open across the indented continuation so the head
/// remains a single tuple pat rather than terminating after `x`.
#[test]
fn diff_ast_let_tuple_value_head_multiline() {
    assert_asts_match("let x,\n    y = 1, 2\n");
}

/// Phase 6.3 — applPat (constructor / function-form) as a tuple
/// continuation element. FCS's `headBindingPat → applPats (','
/// applPat)+` lets each tuple element be a function-form pattern,
/// not just atomic — so `let y, Some x = e` projects to
/// `Tuple([Named "y"; LongIdent("Some", [Named "x"])])`. The
/// symmetric `let Some x, y = e` already works because the first
/// element is treated as function-form by `parse_head_binding_pat`;
/// this pins the same shape on the continuation side.
#[test]
fn diff_ast_let_tuple_value_head_constructor_tail() {
    assert_asts_match("let y, Some x = 1, Some 2\n");
}

/// Phase 6.3 — applPat as the *first* tuple element (symmetric
/// baseline for `diff_ast_let_tuple_value_head_constructor_tail`).
/// Already works via the first-element function-form branch; the
/// test pins the shape so a future refactor that unifies first and
/// continuation handling doesn't regress it.
#[test]
fn diff_ast_let_tuple_value_head_constructor_head() {
    assert_asts_match("let Some x, y = Some 1, 2\n");
}

/// Phase 6.3 — applPat (constructor / function-form) as a paren-tuple
/// element. FCS's grammar inside `parenPattern` lets each tuple
/// element be an applPat, so `let (x, Some y) = e` projects to
/// `Paren(Tuple([Named "x"; LongIdent("Some", [Named "y"])]))`. The
/// in-paren tuple loop needs the same applPat treatment as the
/// top-level `maybe_wrap_tuple_pat` path, plus the per-element
/// typed-pat wrap (so `(x, Some y : int option)` would project to
/// `Paren(Tuple([Named "x"; Typed(LongIdent("Some", [Named "y"]),
/// int option)]))`).
#[test]
fn diff_ast_let_paren_tuple_constructor_tail() {
    assert_asts_match("let (x, Some y) = 1, Some 2\n");
}

/// Phase 6.3 — applPat as the *first* paren-tuple element (symmetric
/// baseline). FCS produces
/// `Paren(Tuple([LongIdent("Some", [Named "x"]); Named "y"]))`.
#[test]
fn diff_ast_let_paren_tuple_constructor_head() {
    assert_asts_match("let (Some x, y) = Some 1, 2\n");
}

/// Phase 6 — `let (Some x) = e`: a *single* constructor-application
/// pattern binding (no surrounding tuple). FCS projects the binding head
/// to `Paren(LongIdent("Some", [Named x]))`. Distinct from the
/// paren-tuple-element cases above: here the ctor-app is the whole
/// paren contents, exercising the non-tuple `parse_paren_pat` path.
#[test]
fn diff_ast_let_paren_ctor_app_head() {
    assert_asts_match("let (Some x) = e\n");
}

/// Phase 5 SimplePatsOfPat sub-plan 5.X.1 — uppercase head ident at
/// the value-form head (no curried args) projects to
/// `SynPat.LongIdent(["X"], …, Pats[])`, not `SynPat.Named "X"`. FCS's
/// `atomicPattern → atomicPatternLongIdent` action (`pars.fsy:3805-3818`)
/// routes any ident whose leading character is uppercase (per
/// `String.isLeadingIdentifierCharacterUpperCase`,
/// `Utilities/illib.fs:740`) through `mkSynPatMaybeVar`, which always
/// constructs `SynPat.LongIdent`. The classifier is the same for nullary
/// constructor patterns (`let None = …`), nullary DU sentinels, and
/// uppercase value bindings — they all share the LongIdent shape.
#[test]
fn diff_ast_let_uppercase_value_head() {
    assert_asts_match("let X = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — `let None = Some 0` exercises the
/// nullary-DU-case pattern. Same shape as the uppercase-value variant
/// above; here the name (`None`) is a real DU case that resolves at
/// typecheck, but the parser only sees an uppercase ident and emits
/// `LongIdent(["None"], …, Pats[])` regardless. The RHS is a
/// constructor-app expression that hits a different code path, so this
/// test pins the value-head LongIdent shape without coupling to the
/// expression-side LongIdent treatment.
#[test]
fn diff_ast_let_none_value_head() {
    assert_asts_match("let None = Some 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — paren-wrapped uppercase head. FCS
/// projects to `Paren(LongIdent(["X"], …, Pats[]))`; the paren survives
/// and the inner uppercase ident still triggers the LongIdent shape.
#[test]
fn diff_ast_let_paren_uppercase_value_head() {
    assert_asts_match("let (X) = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — uppercase head inside a top-level
/// tuple. FCS produces `Tuple([LongIdent("X", …); Named "y"])`; this
/// pins that the tuple-continuation arm of `maybe_wrap_tuple_pat`
/// classifies its head ident the same way as the leading element.
#[test]
fn diff_ast_let_tuple_uppercase_then_lowercase_head() {
    assert_asts_match("let X, y = 0, 1\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — uppercase head in a paren-tuple.
/// Same LongIdent classifier, exercised through `parse_paren_pat`'s
/// in-paren tuple loop. FCS:
/// `Paren(Tuple([LongIdent("X", …); Named "y"]))`.
#[test]
fn diff_ast_let_paren_tuple_uppercase_head() {
    assert_asts_match("let (X, y) = 0, 1\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — quoted-ident with uppercase
/// content. FCS's `Ident.idText` holds the unescaped form, so
/// `` ``Foo`` `` classifies on `F` (uppercase) and projects to
/// `LongIdent(["Foo"], …, Pats[])`. The classifier must strip the
/// backticks before inspecting the leading character — otherwise the
/// `` ` `` itself (non-letter) wins and the ident wrongly falls
/// through to `Named`.
#[test]
fn diff_ast_let_quoted_uppercase_value_head() {
    assert_asts_match("let ``Foo`` = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — quoted-ident with lowercase
/// content stays `Named`. Pins the negative side of the backtick
/// strip: `` ``foo`` `` must not be misclassified as uppercase just
/// because the source includes non-letter `` ` `` characters.
#[test]
fn diff_ast_let_quoted_lowercase_value_head() {
    assert_asts_match("let ``foo`` = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — prime-suffixed lowercase ident
/// (`x'`). The leading character is `x` (lowercase), so the classifier
/// stays on the `Named` arm. Pins that the prime-tail doesn't perturb
/// the leading-character inspection.
#[test]
fn diff_ast_let_prime_lowercase_value_head() {
    assert_asts_match("let x' = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — single-letter uppercase ident
/// remains LongIdent. Just a size-1 edge case for the helper —
/// `s[..1]` boundary is the same as the multi-char case but it's
/// worth pinning the literal one-char path because the classifier's
/// `chars().next()` could regress on empty/single-character inputs.
#[test]
fn diff_ast_let_single_char_uppercase_value_head() {
    assert_asts_match("let A = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — Unicode `Nl` (Letter_Number) head:
/// `Ⅷ` (U+2167 ROMAN NUMERAL EIGHT) is in Unicode general category
/// `Nl` and has the `Other_Uppercase` property. FCS's classifier uses
/// `Char.IsLetter` (`illib.fs:740`) which restricts to `Lu|Ll|Lt|Lm|Lo`
/// and thus returns `false` for `Nl`, so FCS lowers to `Named "Ⅷ"`.
/// Rust's `is_uppercase()` returns `true` for `Ⅷ` (because of
/// `Other_Uppercase`), so the naïve `is_alphabetic()` fallback would
/// misclassify as `LongIdent`. Pins the .NET-aligned letter check.
#[test]
fn diff_ast_let_nl_roman_numeral_value_head() {
    assert_asts_match("let \u{2167} = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — uncased `Nl` head: `ᛮ` (U+16EE
/// RUNIC ARLAUG SYMBOL) is in `Nl` and not cased at all. Rust's
/// `is_alphabetic()` returns `true` (`Nl` is part of the Unicode
/// `Alphabetic` property) so the bicameral fallback misclassifies as
/// uppercase; .NET's `Char.IsLetter` returns `false`, so FCS lowers
/// to `Named "ᛮ"`. Complements the Roman-numeral case for the
/// not-cased branch of the `Nl` exclusion.
#[test]
fn diff_ast_let_nl_runic_value_head() {
    assert_asts_match("let \u{16EE} = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — quoted `Other_Alphabetic`
/// non-letter head: `U+0345` COMBINING GREEK YPOGEGRAMMENI is `Mn`
/// with the derived `Other_Alphabetic` property. Rust's
/// `char::is_alphabetic()` returns `true`, but .NET's `Char.IsLetter`
/// returns `false`, so FCS lowers to `Named`. This must stay classified
/// from the Unicode general category, not Rust's derived alphabetic
/// predicate.
#[test]
fn diff_ast_let_quoted_other_alphabetic_mark_value_head() {
    assert_asts_match("let ``\u{0345}`` = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — quoted `Other_Alphabetic` symbol
/// head: `Ⓐ` (U+24B6 CIRCLED LATIN CAPITAL LETTER A) is `So`, with
/// `Other_Uppercase` / `Other_Alphabetic`. FCS's category-based
/// `Char.IsLetter` rejects it, so it is `Named`, not `LongIdent`.
#[test]
fn diff_ast_let_quoted_other_alphabetic_symbol_value_head() {
    assert_asts_match("let ``\u{24B6}`` = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — `Lm` modifier letter head with
/// `Other_Lowercase`: `ᵃ` (U+1D43 MODIFIER LETTER SMALL A) is in
/// general category `Lm`. .NET's `Char.IsLower` is `Ll`-only, so
/// `IsLower(ᵃ) = false`; FCS falls through to `IsLetter(ᵃ) = true`
/// (Lm is a letter) and classifies as uppercase → `LongIdent`.
/// Rust's `is_lowercase()` returns `true` because Lm modifier
/// letters in the `1D2C..1D6A` block carry the `Other_Lowercase`
/// derived property. Without subtracting `Other_Lowercase` from the
/// case check we would emit `Named` instead.
#[test]
fn diff_ast_let_lm_modifier_value_head() {
    assert_asts_match("let \u{1D43} = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — `Lo` letter head with
/// `Other_Lowercase`: `ª` (U+00AA FEMININE ORDINAL INDICATOR) is in
/// general category `Lo`. .NET's `Char.IsLower` returns `false`
/// (Ll-only); FCS classifies via the `IsLetter` fallback as
/// uppercase → `LongIdent`. Rust's `is_lowercase()` returns `true`
/// (`Other_Lowercase` includes `00AA`). Complements
/// [`diff_ast_let_lm_modifier_value_head`] for the `Lo` branch of
/// the `Other_Lowercase` subtraction.
#[test]
fn diff_ast_let_lo_ordinal_value_head() {
    assert_asts_match("let \u{00AA} = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — singleton `Other_Lowercase`
/// codepoint: `ꭩ` (U+AB69 MODIFIER LETTER SMALL TURNED W) is `Lm`
/// with `Other_Lowercase`. Pins that the table includes the
/// detached `AB69` singleton (separate from the `AB5C..AB5F` block
/// just above it), not just the contiguous runs.
#[test]
fn diff_ast_let_lm_modifier_turned_w_value_head() {
    assert_asts_match("let \u{AB69} = 0\n");
}

/// Phase 5 SimplePatsOfPat 5.X.1 — non-BMP quoted-ident head: `𐐀`
/// (U+10400 DESERET CAPITAL LETTER LONG I) is `Lu` and would be
/// reported uppercase by Rust's `char::is_uppercase`. FCS reads
/// `s[0]` of the .NET string, which is the UTF-16 high surrogate
/// (`U+D801`); `Char.IsUpper`/`IsLower`/`IsLetter` all return
/// `false` for a lone surrogate, so FCS classifies the binder as
/// `Named "𐐀"`. We mirror by short-circuiting on codepoint >=
/// `0x10000` (the BMP boundary) before consulting Rust's character
/// predicates.
#[test]
fn diff_ast_let_quoted_nonbmp_value_head() {
    assert_asts_match("let ``\u{10400}`` = 0\n");
}

/// Phase 6.3 — `let f (x, y) = 1`: tuple pattern as a single
/// function-form argument. FCS produces
/// `SynPat.LongIdent("f", _, _, Pats[Paren(Tuple([Named "x"; Named "y"], …), …)], _, _)`.
/// Confirms the tuple layer fires inside `parse_paren_pat` even when
/// the paren is reached through the function-form curried-arg sweep.
#[test]
fn diff_ast_let_function_form_paren_tuple_arg() {
    assert_asts_match("let f (x, y) = 1\n");
}

/// Phase 6.3 — `let f x, y = 1`: the function-form-vs-tuple-head
/// ambiguity. FCS parses this as a value-form binding whose pat is a
/// tuple of two elements: `LongIdent("f", _, _, Pats[Named "x"])` and
/// `Named "y"`. The function-form head is parsed first (because
/// `f x` matches `applPat`), then the trailing `,` extends it into a
/// tuple — i.e. function-form-then-tuple, not tuple-then-function-
/// form-per-element. Pins our `maybe_wrap_tuple_pat` post-hoc-wrap
/// behaviour against the FCS reference.
#[test]
fn diff_ast_let_function_form_then_tuple_head() {
    assert_asts_match("let f x, y = 1\n");
}

/// Phase 7.9 regression — `let f (x : {| F : int\n              G : string |}) = x`:
/// multi-line anon-recd inside a `let`-binding's typed paren-pat,
/// where LexFilter emits a `Virtual::BlockSep` between the two
/// same-indent fields. FCS's `seps: SEMICOLON | OBLOCKSEP |
/// SEMICOLON OBLOCKSEP` (`pars.fsy:2522`) treats the `OBLOCKSEP`
/// as a field separator, so both implementations must project the
/// same two-field `AnonRecd` shape. Pins our parser's accept-
/// BlockSep behaviour against FCS as the oracle.
#[test]
fn diff_ast_let_binding_with_multi_line_anon_recd_type() {
    assert_asts_match("let f (x : {| F : int\n              G : string |}) = x\n");
}

/// Phase 7.9 regression — `let f (x : {| F : int\n              ; G : string |}) = x`:
/// the `OBLOCKSEP SEMICOLON` separator order. FCS accepts this with
/// no errors (the formal `seps` rule only lists `SEMICOLON OBLOCKSEP`
/// but FCS treats the swapped order as equivalent in practice). Our
/// parser greedily consumes any run of `;` / `BlockSep` tokens as a
/// single separator chunk. Pins agreement with FCS for both orders.
#[test]
fn diff_ast_let_binding_with_block_sep_then_semi_anon_recd_type() {
    assert_asts_match("let f (x : {| F : int\n              ; G : string |}) = x\n");
}

/// Phase 5.X.6 — `let (Some x : int option) = e`: the same typed-ctor paren
/// pattern at a let-binding head (no `parsedData`, so no lowering — pure
/// projection of `Paren(Typed(LongIdent("Some",[x]), int option))`). Pins
/// the typed-non-simple-inner shape at the second caller site.
#[test]
fn diff_ast_let_typed_ctor_value_head() {
    assert_asts_match("let (Some x : int option) = e\n");
}

/// Phase 6 (Gap B) — `let f (x) y = x`: the let-head twin of
/// `diff_ast_fun_lambda_paren_ident_then_simple`. `(x)` is a simple named
/// pat; `y` is a separate curried head arg. Pins the function-form
/// decision gate at the let-head caller site.
#[test]
fn diff_ast_let_function_form_paren_ident_then_simple() {
    assert_asts_match("let f (x) y = x\n");
}

/// Phase 5 Gap A — `let x as y = w`: minimal top-level `as`-pattern.
/// `headBindingPattern AS constrPattern` (`pars.fsy:3570`) projects to
/// `SynPat.As(Named x, Named y)`.
#[test]
fn diff_ast_let_as_pat_minimal() {
    assert_asts_match("let x as y = w\n");
}

/// Phase 5 Gap A — `let x, y as z = w`: `%right AS` is the lowest
/// pattern precedence, so the tuple binds tighter and `as` wraps it →
/// `As(Tuple[x,y], z)`.
#[test]
fn diff_ast_let_as_pat_tuple_lhs() {
    assert_asts_match("let x, y as z = w\n");
}

/// Phase 5 Gap A — `let x as y as z = w`: chained `as` is left-nested
/// (`As(As(x,y),z)`) because FCS's `headBindingPattern AS constrPattern`
/// is left-recursive.
#[test]
fn diff_ast_let_as_pat_chained() {
    assert_asts_match("let x as y as z = w\n");
}

/// Phase 5 Gap A — `let (x as y) = w`: in-paren `as` via `parenPattern
/// AS constrPattern` (`pars.fsy:3902`) → `Paren(As(Named x, Named y))`.
#[test]
fn diff_ast_let_paren_as_pat() {
    assert_asts_match("let (x as y) = w\n");
}

/// Phase 5 Gap A — `let (Some x as y) = w`: the `as` lhs is a
/// function-form `SynPat.LongIdent(Some, [x])`; the rhs stays a simple
/// `Named`.
#[test]
fn diff_ast_let_paren_as_ctor_lhs() {
    assert_asts_match("let (Some x as y) = w\n");
}

/// Phase 5 Gap A — `let (x as Some z) = w`: the `as` rhs is itself a
/// `constrPattern` (`pars.fsy:3902`), not just an ident, so the rhs
/// emitter builds a function-form `SynPat.LongIdent(Some, [z])` →
/// `Paren(As(Named x, LongIdent(Some, [z])))`. Pins that the rhs handles
/// ctor-application binders, the mirror of `diff_ast_let_paren_as_ctor_lhs`.
#[test]
fn diff_ast_let_paren_as_ctor_rhs() {
    assert_asts_match("let (x as Some z) = w\n");
}

/// Phase 5 Gap A — `let (x, y as z) = w`: tuple binds tighter than `as`
/// inside parens → `Paren(As(Tuple[x,y], z))`.
#[test]
fn diff_ast_let_paren_as_tuple_lhs() {
    assert_asts_match("let (x, y as z) = w\n");
}

/// Phase 5 Gap A — `let (x : int as y) = w`: a per-element `:` binds the
/// lhs typed-pat, then `as` wraps it → `Paren(As(Typed(x,int), y))`.
#[test]
fn diff_ast_let_paren_as_typed_lhs() {
    assert_asts_match("let (x : int as y) = w\n");
}

/// Phase 5 Gap A — `let (x as y : int) = w`: a trailing `:` after the
/// whole `as` wraps it → `Paren(Typed(As(x,y), int))`. The colon's left
/// operand is the reduced `as`, since `constrPattern` can't absorb it.
#[test]
fn diff_ast_let_paren_as_trailing_colon() {
    assert_asts_match("let (x as y : int) = w\n");
}

/// Phase 5 Gap A — `let x as y, z = w`: the `as` binds only its single
/// lhs (`x`), then the comma builds the surrounding tuple →
/// `Tuple[As(x,y), z]`. The `as`-pat is a tuple *element*, not the whole
/// head — FCS's tuple-reduce (prec `COMMA`) outranks the `AS` shift, so
/// `x as y` reduces before the comma is consumed.
#[test]
fn diff_ast_let_as_then_comma() {
    assert_asts_match("let x as y, z = w\n");
}

/// Phase 5 Gap A — `let (x as y, z) = w`: same interleave inside parens →
/// `Paren(Tuple[As(x,y), z])`.
#[test]
fn diff_ast_let_paren_as_then_comma() {
    assert_asts_match("let (x as y, z) = w\n");
}

/// Phase 5 Gap A — `let x as y, z as w = v`: the first `as` is a tuple
/// element; the second `as` (after the comma-run reduces) wraps the whole
/// tuple → `As(Tuple[As(x,y),z], w)`.
#[test]
fn diff_ast_let_as_comma_as() {
    assert_asts_match("let x as y, z as w = v\n");
}

/// Phase 5 Gap A — `let x, y as z, w = v`: the comma-run `x, y` reduces to
/// a flat tuple before the `as`, which wraps it; a trailing comma then
/// opens a fresh outer tuple → `Tuple[As(Tuple[x,y],z], w]`.
#[test]
fn diff_ast_let_comma_as_comma() {
    assert_asts_match("let x, y as z, w = v\n");
}

/// Phase 5 Gap A — `let (x : int as y, z) = w`: the per-element `:` binds
/// `x` (`Typed(x,int)`), `as` wraps that, then the comma builds the tuple →
/// `Paren(Tuple[As(Typed(x,int),y), z])`.
#[test]
fn diff_ast_let_paren_typed_as_comma() {
    assert_asts_match("let (x : int as y, z) = w\n");
}

/// Phase 5 Gap A — `let (x as y : int, z) = w`: `as` binds `x`, the
/// trailing `:` wraps the whole `as` (`Typed(As(x,y),int)`), then the comma
/// builds the tuple → `Paren(Tuple[Typed(As(x,y),int), z])`.
#[test]
fn diff_ast_let_paren_as_typed_comma() {
    assert_asts_match("let (x as y : int, z) = w\n");
}

/// Phase 6 — `let [x; y] = z`: minimal list pattern →
/// `SynPat.ArrayOrList(false, [Named x; Named y])`.
#[test]
fn diff_ast_let_list_pat() {
    assert_asts_match("let [x; y] = z\n");
}

/// Phase 6 — `let [| x; y |] = z`: minimal array pattern →
/// `SynPat.ArrayOrList(true, [Named x; Named y])`.
#[test]
fn diff_ast_let_array_pat() {
    assert_asts_match("let [| x; y |] = z\n");
}

/// Phase 6 — `let [] = z`: empty list pattern (valid) →
/// `SynPat.ArrayOrList(false, [])`.
#[test]
fn diff_ast_let_list_pat_empty() {
    assert_asts_match("let [] = z\n");
}

/// Phase 6 — `let [||] = z`: empty array pattern (valid) →
/// `SynPat.ArrayOrList(true, [])`.
#[test]
fn diff_ast_let_array_pat_empty() {
    assert_asts_match("let [||] = z\n");
}

/// Phase 6 — `let [x] = z`: single-element list.
#[test]
fn diff_ast_let_list_pat_single() {
    assert_asts_match("let [x] = z\n");
}

/// Phase 6 — `let [a, b] = z`: the `,` builds a tuple *within* the single
/// element, so the list has one `SynPat.Tuple` element, not two
/// (`;` is the element separator).
#[test]
fn diff_ast_let_list_pat_tuple_element() {
    assert_asts_match("let [a, b] = z\n");
}

/// Phase 6 — `let [[x]; y] = z`: a nested list pattern as the first
/// element.
#[test]
fn diff_ast_let_list_pat_nested() {
    assert_asts_match("let [[x]; y] = z\n");
}

/// Phase 6 — `let [Some x; None] = z`: each element is an applPat, so the
/// ctor applications project to `SynPat.LongIdent`.
#[test]
fn diff_ast_let_list_pat_ctor_element() {
    assert_asts_match("let [Some x; None] = z\n");
}

/// Phase 6 — `let f [x] = x`: a bracket arg promotes the head to
/// function form → `SynPat.LongIdent(f, [ArrayOrList(false, [Named x])])`.
#[test]
fn diff_ast_let_function_form_list_arg() {
    assert_asts_match("let f [x] = x\n");
}

/// Phase 6 — `let [x; y] as z = w`: `as` is the lowest pattern
/// precedence, so it wraps the whole atomic list →
/// `As(ArrayOrList(false, [x; y]), Named z)`.
#[test]
fn diff_ast_let_list_pat_as() {
    assert_asts_match("let [x; y] as z = w\n");
}

/// Phase 6 — offside-separated list elements. The layout virtual between
/// `x` and `y` is the element separator; both must stay distinct
/// `SynPat.Named`s rather than `x` being promoted to function form.
#[test]
fn diff_ast_let_list_pat_offside() {
    assert_asts_match("let [ x\n      y ] = z\n");
}

/// Phase 6.5 — a record pattern as a *list element* followed by another
/// element: `[ { X = a }; y ]`. Same swallowed-`}`-before-outer-`;` hazard as
/// `diff_ast_match_record_pat_nested_then_field`, but the outer separator
/// belongs to the enclosing list rather than another record field.
#[test]
fn diff_ast_let_list_record_element_then_element() {
    assert_asts_match("let [ { X = a }; y ] = z\n");
}

/// Phase 6.5 — a record pattern at a `let` head: `let { X = a } = r`.
/// Exercises the `raw_starts_atomic_pat` hook so a `{` is recognised as a
/// head-binding pattern start.
#[test]
fn diff_ast_let_record_pat() {
    assert_asts_match("let { X = a } = r\n");
}

/// Phase 6.6 — an IsInst pattern in a parenthesised `let` head: `let (:? int)
/// = x`. FCS preserves the `Paren` wrapper, so the head projects to
/// `Paren(IsInst(int))`. Exercises the paren-element pattern path
/// (`emit_paren_pat_element`).
#[test]
fn diff_ast_let_paren_isinst_head() {
    assert_asts_match("let (:? int) = x\n");
}

/// Phase 6.7 — a cons pattern at a `let` head: `let h :: t = z` ⇒ head
/// `ListCons(h, t)` (refutable, but FCS parses it).
#[test]
fn diff_ast_let_cons_pat() {
    assert_asts_match("let h :: t = z\n");
}

/// Phase 6.7 — a parenthesised cons at a `let` head: `let (h :: t) = z` ⇒
/// `Paren(ListCons(h, t))`. Exercises the in-paren climber (cons inside the
/// paren-element path).
#[test]
fn diff_ast_let_paren_cons_pat() {
    assert_asts_match("let (h :: t) = z\n");
}

/// Phase 6.7 — `::` binds tighter than the per-element paren `:`:
/// `(a :: b : int)` ⇒ `Paren(ListCons(a, Typed(b, int)))` — the `: int`
/// attaches to the cons rhs element `b` (consumed per-element), not the whole
/// cons.
#[test]
fn diff_ast_let_paren_cons_typed_element() {
    assert_asts_match("let (a :: b : int) = z\n");
}

/// Phase 6.8 — a conjunction at a `let` head: `let a & b = z` ⇒ head
/// `Ands[a, b]` (refutable, but FCS parses it).
#[test]
fn diff_ast_let_ands_pat() {
    assert_asts_match("let a & b = z\n");
}

/// Phase 6.8 — a parenthesised conjunction at a `let` head:
/// `let (a & b) = z` ⇒ `Paren(Ands[a, b])`. Exercises the in-paren climber.
#[test]
fn diff_ast_let_paren_ands_pat() {
    assert_asts_match("let (a & b) = z\n");
}

/// Phase 6.8 — the per-element `:` binds tighter than `&` inside parens:
/// `(a & b : int)` ⇒ `Ands[a, Typed(b, int)]` — the `: int` attaches to the
/// conjunction's second operand `b` (consumed per-element), not the whole
/// `Ands`. (The same subtlety as `(a :: b : int)`.)
#[test]
fn diff_ast_let_paren_ands_typed_element() {
    assert_asts_match("let (a & b : int) = z\n");
}

/// Phase 6.9 — an or-pattern at a `let` head: `let A | B = z` ⇒ head
/// `Or(A, B)` (refutable, but FCS parses it).
#[test]
fn diff_ast_let_or_pat() {
    assert_asts_match("let A | B = z\n");
}

/// Phase 6.9 — a parenthesised or-pattern at a `let` head: `let (A | B) = z`
/// ⇒ `Paren(Or(A, B))`. Exercises the in-paren climber.
#[test]
fn diff_ast_let_paren_or_pat() {
    assert_asts_match("let (A | B) = z\n");
}

/// Stage 3 — explicit `;` sequential on a `let` RHS: `let x = printf "a"; 1`.
/// FCS binds `x` to `SynExpr.Sequential(App(printf, "a"), Const(Int32 1))`.
/// Previously rejected ("unexpected token after binding expression"); the
/// binding RHS now routes through the shared seq-block gatherer.
#[test]
fn diff_ast_let_rhs_semi_sequential() {
    assert_asts_match("let x = printf \"a\"; 1\n");
}

/// Stage 3 — offside multi-statement function body:
/// `let f y =⏎    printf "a"⏎    y`. FCS binds the RHS to
/// `Sequential(App(printf, "a"), Ident y)`.
#[test]
fn diff_ast_let_rhs_offside_sequential() {
    assert_asts_match("let f y =\n    printf \"a\"\n    y\n");
}

/// Stage 3 — three-statement offside RHS flattens to one n-ary `Sequential`
/// (FCS's right-leaning `Sequential(a, Sequential(b, c))` normalises to the
/// same list).
#[test]
fn diff_ast_let_rhs_offside_three() {
    assert_asts_match("let x =\n    a\n    b\n    c\n");
}

/// Stage 3 — a three-way `;` run on a `let` RHS flattens to one n-ary
/// `Sequential` (`let x = a; b; c`).
#[test]
fn diff_ast_let_rhs_semi_three() {
    assert_asts_match("let x = a; b; c\n");
}

/// Stage 3 — a sequential RHS whose first statement is itself a block-opening
/// control-flow expr: the gatherer must resume at the same offside level after
/// the `if`'s own block closes. `let x =⏎  if c then 1 else 2⏎  3` ⇒
/// `Sequential(IfThenElse(c, 1, 2), 3)`.
#[test]
fn diff_ast_let_rhs_seq_with_nested_if() {
    assert_asts_match("let x =\n    if c then 1 else 2\n    3\n");
}

/// Stage 3 — a trailing `;` on a `let` RHS does not sequence: FCS parses
/// `let x = a;` with a bare `Ident a` RHS and no error (the dangling
/// separator is dropped). The gatherer consumes the `;` as a `SEMI_TOK` but
/// finds no following statement, so it leaves the RHS a single expression.
#[test]
fn diff_ast_let_rhs_trailing_semi() {
    assert_asts_match("let x = a;\n");
}

/// Parenthesised sequential — `(a; b)`: FCS's `parenExprBody` is a full
/// `typedSequentialExpr`, so the body sequences. Shape:
/// `Paren(Sequential(Ident a, Ident b))`. `parse_paren_expr` routes its
/// inner expression through the shared seq-block gatherer (the `)` is
/// LexFilter-swallowed and there are no wrapping block virtuals inside the
/// parens, so the gatherer stops naturally and `bump_swallowed_rparen`
/// still consumes the `)`).
#[test]
fn diff_ast_paren_semi_sequential() {
    assert_asts_match("let z = (a; b)\n");
}

/// Parenthesised sequential, three statements — flattens to one n-ary
/// `Paren(Sequential(a, b, c))` (FCS's right-leaning chain normalises to the
/// same list).
#[test]
fn diff_ast_paren_semi_sequential_three() {
    assert_asts_match("let z = (a; b; c)\n");
}

/// Parenthesised sequential, multi-line — the separator is an offside
/// `Virtual::BlockSep` rather than an explicit `;`, but the gatherer treats
/// both forms identically. `Paren(Sequential(a, b))`.
#[test]
fn diff_ast_paren_offside_sequential() {
    assert_asts_match("let z = (\n    a\n    b\n)\n");
}

/// Parenthesised sequential whose statements are themselves tuples:
/// `(a, b; c, d)`. The `,` binds tighter than the `;`, so each sequence
/// statement is a `Tuple`: `Paren(Sequential(Tuple(a, b), Tuple(c, d)))`.
#[test]
fn diff_ast_paren_sequential_of_tuples() {
    assert_asts_match("let z = (a, b; c, d)\n");
}

/// Parenthesised sequential with a trailing type annotation — FCS's
/// `typedSequentialExpr: sequentialExpr COLON typ` binds the `:` to the
/// *whole* sequence, so the shape is `Paren(Typed(Sequential(a, b), int))`.
/// The typed-expression hook wraps the already-built `SEQUENTIAL_EXPR`.
#[test]
fn diff_ast_paren_sequential_typed() {
    assert_asts_match("let z = (a; b : int)\n");
}

/// Regression guard — a single-statement typed paren still projects as
/// `Paren(Typed(a, int))` (no `Sequential` wrapper) after routing the inner
/// expression through the seq-block gatherer.
#[test]
fn diff_ast_paren_single_typed() {
    assert_asts_match("let z = (a : int)\n");
}

/// Trailing separator inside parens — `(a;) + b`. FCS's `declExpr seps` arm
/// treats the `;` as trailing, so `(a;)` is `Paren(a)` and the `+ b` belongs
/// *outside*: `App(App(+, Paren(a)), b)`, no diagnostics. The `)` is
/// LexFilter-swallowed (absent from the filtered stream), so the seq-block
/// gatherer must consult the raw stream after consuming the `;` and refuse to
/// pull the outer `+ b` in as a second statement — otherwise the real `)`
/// would land in the tree as ERROR.
#[test]
fn diff_ast_paren_trailing_semi_then_infix() {
    assert_asts_match("let z = (a;) + b\n");
}

/// Trailing separator after a *multi-statement* paren body — `(a; b;) + c`
/// ⇒ `App(App(+, Paren(Sequential(a, b))), c)`. The trailing `;` before the
/// swallowed `)` is dropped; the gatherer stops there and the outer `+ c`
/// continues at the Pratt layer.
#[test]
fn diff_ast_paren_sequential_trailing_semi_then_infix() {
    assert_asts_match("let z = (a; b;) + c\n");
}

/// A multiline parenthesised expression followed by an *outer* same-indent
/// statement: the RHS is `Sequential(Paren(a), b)`, not a paren that swallows
/// `b`. LexFilter emits the outer `Virtual::BlockSep` (between the paren expr
/// and `b`) while the swallowed `)` is still pending on the raw stream, so the
/// in-paren gatherer must refuse that separator and let the *enclosing* let-RHS
/// gatherer consume it. (Regression: an earlier raw-close guard that ran only
/// after consuming the separator pulled `b` into the parens and dropped the
/// real `)` as ERROR.)
#[test]
fn diff_ast_paren_then_outer_offside_stmt() {
    assert_asts_match("let x =\n    (a\n    )\n    b\n");
}

/// Accessibility on a value-form binding head — `let private x = 1`. FCS's
/// `atomicPatternLongIdent: access pathOp` attaches the modifier to the head
/// pattern: `SynPat.Named(SynIdent("x"), false, Some(Private …), …)`. We
/// consume `private`/`internal`/`public` as a sibling `ACCESS_TOK` of the
/// `NAMED_PAT` (elided by the normaliser, like every other accessibility
/// site), so the projected shape is the bare `NormalisedPat::Named("x")`
/// FCS also elides to. Regression: before this the access keyword landed where
/// a pattern head was expected and produced "expected pattern after `let`".
#[test]
fn diff_ast_let_private_value() {
    assert_asts_match("let private x = 1\n");
}

/// As [`diff_ast_let_private_value`] for `internal`.
#[test]
fn diff_ast_let_internal_value() {
    assert_asts_match("let internal y = 2\n");
}

/// As [`diff_ast_let_private_value`] for `public`.
#[test]
fn diff_ast_let_public_value() {
    assert_asts_match("let public z = 3\n");
}

/// Accessibility on a *function-form* binding head — `let private f a = a`.
/// FCS attaches the modifier to `SynPat.LongIdent(SynLongIdent(["f"]), …, args,
/// Some(Private …), …)`; the `ACCESS_TOK` is a sibling of the `LONG_IDENT_PAT`,
/// so the projected `NormalisedPat::LongIdent { head: ["f"], args: [Named "a"] }`
/// matches FCS (accessibility elided on both sides).
#[test]
fn diff_ast_let_private_function() {
    assert_asts_match("let private f a = a\n");
}

/// Accessibility composes with `rec` — `let rec private f x = x`. FCS's
/// `defnBindings: LET opt_rec localBindings` takes `rec` before the binding,
/// and the `private` then rides on the head pattern. The `REC_TOK` (on the
/// `LET_DECL`) and the `ACCESS_TOK` (on the `BINDING`) sit in distinct slots,
/// so both project cleanly (`is_rec = true`, accessibility elided).
#[test]
fn diff_ast_let_rec_private_function() {
    assert_asts_match("let rec private f x = x\n");
}

/// Accessibility before a function head with a parenthesised tuple arg —
/// `let private f (a, b) = a`. Confirms the modifier consumption leaves the
/// function-form promotion + paren-arg machinery untouched: the head projects
/// to `LongIdent { head: ["f"], args: [Paren(Tuple [Named a, Named b])] }`.
#[test]
fn diff_ast_let_private_function_tuple_arg() {
    assert_asts_match("let private f (a, b) = a\n");
}

/// Accessibility on a *non-first* tuple binding element — `let a, private b`.
/// FCS's `access pathOp` is a per-element production (not just the head), so the
/// modifier rides on the second `SynPat.Named`'s accessibility slot (elided).
/// The modifier is not a pattern *start*, so the tuple-element emitter must admit
/// it before dispatching to the head-element parser that consumes it.
#[test]
fn diff_ast_tuple_element_private() {
    assert_asts_match("let a, private b = 1, 2\n");
}

/// Accessibility on *both* tuple elements — `let private a, private b` (the
/// `Named 06` corpus shape). The leading `private` rides on the head, the second
/// on the comma element.
#[test]
fn diff_ast_tuple_both_elements_private() {
    assert_asts_match("let private a, private b = 1, 2\n");
}

/// `internal` (not just `private`) on a tuple element — `let a, internal b`.
#[test]
fn diff_ast_tuple_element_internal() {
    assert_asts_match("let a, internal b = 1, 2\n");
}

/// Accessibility on a `::`-cons element — `let a :: private b = []`. The cons rhs
/// element goes through the same admit-then-consume path.
#[test]
fn diff_ast_cons_element_private() {
    assert_asts_match("let a :: private b = []\n");
}

/// Accessibility on a *parenthesised* tuple element — `let (a, private b)`. The
/// paren-element emitter homes on the same head-element parser, so the modifier
/// is consumed there too.
#[test]
fn diff_ast_paren_tuple_element_private() {
    assert_asts_match("let (a, private b) = 1, 2\n");
}

/// Accessibility on a *match-clause* tuple element — `| a, private b -> …`.
/// Parse-accepted by FCS (the modifier is meaningless in a match pattern but the
/// grammar admits `access pathOp` there), so we mirror it.
#[test]
fn diff_ast_clause_tuple_element_private() {
    assert_asts_match("let f x = match x with | a, private b -> 1 | _ -> 2\n");
}

/// Return-type annotation on a value binding head — `let bar: int = 1`.
/// FCS keeps `headPat = Named "bar"` (the colon is *not* a typed-pat) and
/// records the type twice: in `SynBinding.returnInfo` *and* by wrapping the
/// RHS in `SynExpr.Typed(Const 1, int)`. Our projection elides `returnInfo`
/// (matching how the FCS side reads only the `expr` field) and synthesises
/// the same `Typed` wrapper from the `BINDING_RETURN_INFO` node, so both
/// sides land on `expr = Typed { expr: Const(Int32 1), ty: LongIdent ["int"] }`.
#[test]
fn diff_ast_let_value_return_type() {
    assert_asts_match("let bar: int = 1\n");
}

/// Return type with the `mutable` modifier — `let mutable bar: int = 1`.
/// Pins that the modifier consumption and the return-type consumption
/// compose (FCS: `isMutable = true`, same `Typed` RHS wrapper).
#[test]
fn diff_ast_let_mutable_return_type() {
    assert_asts_match("let mutable bar: int = 1\n");
}

/// Return type on a *function*-form head — `let f x : int = 1`. The
/// curried-arg sweep stops at the `:` (not an atomic-pat start), so the type
/// attaches as the binding's return info, not as a parameter's typed-pat.
/// FCS: `headPat = LongIdent(f, [x])`, RHS `Typed(Const 1, int)`.
#[test]
fn diff_ast_let_function_return_type() {
    assert_asts_match("let f x : int = 1\n");
}

/// A non-trivial return type — `let bar: int -> int = id`. Confirms the
/// full `parse_type` surface (here a function type) reaches the binding
/// return-info site, matching FCS's `Typed(Ident id, Fun(int, int))`.
#[test]
fn diff_ast_let_return_type_function_type() {
    assert_asts_match("let bar: int -> int = id\n");
}

/// A *named* parameter in a binding return-type annotation — `let f : x: int ->
/// int = …`. The return-info production `opt_topReturnTypeWithTypeConstraints`
/// (`pars.fsy:6039`) is a `topType` context, so the labelled argument lowers to a
/// `SynType.SignatureParameter` (FCS emits one here just as in a `.fsi` val sig).
#[test]
fn diff_ast_let_return_type_named_param() {
    assert_asts_match("let f : x: int -> int = fun y -> y\n");
}

// ============================================================================
// Expression-level block `let`/`use` — `SynExpr.LetOrUse` (non-bang form).
// ============================================================================
//
// An offside `let`/`use` in *expression* position (a function/`let` body, a
// `fun`/`if`/`match` body, a paren body) binds a value and nests the rest of
// the block as its body — FCS's `SynExpr.LetOrUse(SynLetOrUse)` with
// `IsBang = false`. LexFilter already emits the full offside scaffolding (the
// inner `Virtual::Let`, the binding's `BlockBegin…BlockEnd`, `DeclEnd`, the
// body-separator `BlockSep`); the parser dispatches it in `parse_minus_expr`
// (beside the `let!`/`use!` `Virtual::Binder` arm) into the shared
// `LET_OR_USE_EXPR` node. The body itself goes through `parse_seq_block_body`,
// so it may be another `let` (nesting) or a multi-statement `Sequential`.

/// The motivating case — a function whose body is `let bar = 3` followed by
/// the body `bar`. FCS: the binding's RHS is `SynExpr.LetOrUse([bar = 3],
/// body = Ident bar)`; the whole thing is **one** `LetOrUse`, not a
/// `Sequential` of the binding and `bar`.
#[test]
fn diff_ast_block_let_in_function_body() {
    assert_asts_match("let foo () =\n    let bar = 3\n    bar\n");
}

/// `use` binding in expression position — `SynLetOrUse` with the head
/// binding's leading keyword `Use` (not `Let`). The `use`/`let` distinction
/// rides on the `LET_TOK` text, exactly as the module-level `LetDecl`.
#[test]
fn diff_ast_block_use_in_function_body() {
    assert_asts_match("let foo () =\n    use a = r ()\n    a\n");
}

/// `let rec … and …` in expression position — one `LetOrUse` with
/// `IsRecursive = true` holding **both** bindings (`SynLetOrUse.Bindings`),
/// the head keyed `LetRec` and the follower `And`. Exercises the
/// `next_token_past_rhs_close_is(Token::And)` and-chain inside the body.
#[test]
fn diff_ast_block_let_rec_and() {
    assert_asts_match("let foo () =\n    let rec a = 1\n    and b = 2\n    a\n");
}

/// Two block-`let`s in sequence nest as `LetOrUse([a], LetOrUse([b], body))` —
/// the second `let` is the *body* of the first, parsed recursively through
/// `parse_seq_block_body`, **not** a sibling statement.
#[test]
fn diff_ast_block_let_nested() {
    assert_asts_match("let foo () =\n    let a = 1\n    let b = 2\n    a\n");
}

/// A non-`let` statement *then* a block-`let`: the body is
/// `Sequential([printfn …, LetOrUse([a], body = a)])`. Pins that the gatherer
/// keeps the leading expression a sibling while the trailing `let` absorbs the
/// remainder as its body.
#[test]
fn diff_ast_block_let_after_expr() {
    assert_asts_match("let foo () =\n    printfn \"x\"\n    let a = 1\n    a\n");
}

/// Explicit-`in` body on one line — `let a = 1 in a`. LexFilter surfaces no
/// body-separator `BlockSep` here; `close_binder_binding` claims the raw `in`
/// as `IN_TOK` and the body follows directly. Same `LetOrUse` shape as the
/// offside form.
#[test]
fn diff_ast_block_let_explicit_in() {
    assert_asts_match("let foo () =\n    let a = 1 in a\n");
}

/// `let mutable` in expression position — the binding-modifier path composes
/// with the new production (`isMutable = true` on the binding).
#[test]
fn diff_ast_block_let_mutable() {
    assert_asts_match("let foo () =\n    let mutable a = 1\n    a\n");
}

/// Block-`let` inside a `fun` body — confirms the dispatch fires in every
/// expression context, not just a `let` RHS (the lambda body is the same
/// `parse_seq_block_body`).
#[test]
fn diff_ast_block_let_in_fun_body() {
    assert_asts_match("let foo = fun () ->\n    let a = 1\n    a\n");
}

/// Block-`let` inside an `if` branch — the then-branch body is
/// `LetOrUse([a], body = a)`. Exercises the production from `parse_if_body`.
#[test]
fn diff_ast_block_let_in_if_branch() {
    assert_asts_match("let foo b =\n    if b then\n        let a = 1\n        a\n    else 0\n");
}

/// Block-`let` inside a `match` arm body — the arm body is
/// `LetOrUse([x], body = x)`. The arm bodies share the same
/// `parse_seq_block_body` gather as `if`/`fun`, so the production fires here
/// too without arm-specific handling.
#[test]
fn diff_ast_block_let_in_match_arm() {
    assert_asts_match(
        "let foo a =\n    match a with\n    | 0 ->\n        let x = 1\n        x\n    | _ -> 2\n",
    );
}

/// Virtual-drain regression: a function with a block-`let` body **followed by
/// a sibling top-level `let`**. The body's `LetOrUse` must stop at the
/// function's own `BlockEnd`; over-draining would swallow the function's close
/// virtuals and drop `let bar = 2` from the module. Pins the single-pair drain
/// discipline.
#[test]
fn diff_ast_block_let_then_sibling_decl() {
    assert_asts_match("let foo () =\n    let a = 1\n    a\nlet bar = 2\n");
}

/// A parenthesised block `let … in …` — `(let a = 1 in a)`. LexFilter emits
/// `Raw(LParen), Virtual(Let)`, so the raw token past the `(` is `Token::Let`
/// (which `raw_starts_minus_expr` excludes); the `peek_is_expr_start` /
/// `parse_atomic_expr` LParen lookaheads admit it explicitly so the paren-expr
/// body (`parse_seq_block_body`) reaches the `Virtual::Let` production. FCS:
/// `Paren(LetOrUse([a], body = a))`.
#[test]
fn diff_ast_paren_let_in() {
    assert_asts_match("let z = (let a = 1 in a)\n");
}

/// Offside (no explicit `in`) parenthesised block `let` — `(let a = 1⏎ a)`.
/// Same `Paren(LetOrUse(...))` shape; exercises the body-separator path inside
/// parens (the gatherer stops at the LexFilter-swallowed `)`).
#[test]
fn diff_ast_paren_let_offside() {
    assert_asts_match("let foo () =\n    (let a = 1\n     a)\n");
}

/// Parenthesised `use … in …` — `(use a = r () in a)`. The `use` keyword
/// (`Token::Use`) is admitted after `(` alongside `let`.
#[test]
fn diff_ast_paren_use_in() {
    assert_asts_match("let z = (use a = r () in a)\n");
}

/// A parenthesised block `let` as a *function argument* — `f (let x = 1 in x)`.
/// The application-layer `(`-lookahead (`peek_starts_app_arg`) shares the same
/// predicate as the expression-start ones, so the paren-let is recognised as an
/// argument rather than stranding `f`. FCS: `App(f, Paren(LetOrUse([x], x)))`.
#[test]
fn diff_ast_paren_let_as_app_arg() {
    assert_asts_match("let r = f (let x = 1 in x)\n");
}

/// The *adjacent* high-precedence form `f(let x = 1 in x)` — no space, so
/// LexFilter inserts `HighPrecedenceParenApp`. `peek_high_precedence_paren_app`
/// shares the predicate too, so the adjacent paren-let application parses.
#[test]
fn diff_ast_paren_let_as_adjacent_app_arg() {
    assert_asts_match("let r = f(let x = 1 in x)\n");
}

/// Two paren-let arguments in one application — `g (let a = 1 in a)
/// (let b = 2 in b)`. Pins that consuming one paren-let argument leaves the
/// app loop able to recognise the next.
#[test]
fn diff_ast_paren_let_two_app_args() {
    assert_asts_match("let r = g (let a = 1 in a) (let b = 2 in b)\n");
}

// --- Attributes on a binding (`let [<Attr>] x = …`) -------------------------
// FCS's `localBinding: attributes opt_access opt_inline opt_mutable
// headBindingPattern` allows an attribute run *between* the `let`/`and` keyword
// and the binding pattern; they land in `SynBinding.attributes` (field 4), the
// same slot as the pre-`let` form (`[<Attr>] let x = …`). Parsed as
// `ATTRIBUTE_LIST` children of the `BINDING` node.

/// The motivating case — `let [<Literal>] x = 1`: an attribute between `let` and
/// the value pattern. FCS: `SynBinding` with `attributes = [[Literal]]`.
#[test]
fn diff_ast_let_binding_attribute() {
    assert_asts_match("let [<Literal>] x = 1\n");
}

/// Attribute on a *function*-form binding — `let [<EntryPoint>] main args = 0`.
#[test]
fn diff_ast_let_binding_attribute_function() {
    assert_asts_match("let [<EntryPoint>] main args = 0\n");
}

/// Attribute then accessibility — `let [<Literal>] private x = 1` (FCS's
/// `attributes opt_access`). The attribute run precedes the `private`.
#[test]
fn diff_ast_let_binding_attribute_private() {
    assert_asts_match("let [<Literal>] private x = 1\n");
}

/// An arg-bearing attribute — `let [<CompiledName("X")>] x = 1`. Exercises the
/// attribute argument expression in the binding-attribute position.
#[test]
fn diff_ast_let_binding_attribute_with_arg() {
    assert_asts_match("let [<CompiledName(\"X\")>] x = 1\n");
}

/// Two adjacent attribute lists — `let [<A>] [<B>] x = 1` (two
/// `SynAttributeList`s on the one binding).
#[test]
fn diff_ast_let_binding_two_attribute_lists() {
    assert_asts_match("let [<A>] [<B>] x = 1\n");
}

/// Attribute composes with `inline` — `let [<Foo>] inline f x = x` (FCS's
/// `attributes opt_access opt_inline`: the run precedes the modifier).
#[test]
fn diff_ast_let_binding_attribute_inline() {
    assert_asts_match("let [<Foo>] inline f x = x\n");
}

/// Attribute on an `and`-chained binding — `let rec f x = 1 and [<TailCall>] g x
/// = 2`. The attributes attach to *that* binding (the second), not the first.
#[test]
fn diff_ast_and_binding_attribute() {
    assert_asts_match("let rec f x = 1\nand [<TailCall>] g x = 2\n");
}

/// Both forms at once — `[<A>] let [<B>] x = 1`. FCS concatenates the pre-`let`
/// and post-`let` runs into one `SynBinding.attributes` in source order.
#[test]
fn diff_ast_let_binding_attribute_both_positions() {
    assert_asts_match("[<A>] let [<B>] x = 1\n");
}

/// Attribute on a binding in *expression* position — a block-`let` body binding
/// (`let [<Literal>] x = 1` inside a function). Confirms the binding-attribute
/// run is parsed wherever a binding is, not only at module level.
#[test]
fn diff_ast_expr_let_binding_attribute() {
    assert_asts_match("let foo () =\n    let [<Literal>] x = 1\n    x\n");
}

// --- Non-block `let … in` as an expression operand --------------------------
// A *mid-expression* `let … in` (not at the head of an offside block) surfaces
// as a *raw* `Token::Let`/`Token::Use` with an explicit `Raw(In)` (or, the
// offside-body form, the body directly after the binding-RHS `BlockEnd`), unlike
// the block-leading form's `Virtual(Let)`. FCS accepts these as
// `SynExpr.LetOrUse` operands; `parse_minus_expr` dispatches the raw keyword the
// same way the block form's virtual is dispatched, so the same `LET_OR_USE_EXPR`
// node (and its keyword-agnostic normalisation) is produced.

/// The motivating case — a `let … in` as the right operand of `&&`:
/// `a && let y = x in y` is `App(&&, [a, LetOrUse([y = x], body = y)])`. The
/// `let` after `&&` is a raw `Token::Let`, reached as the infix RHS operand.
#[test]
fn diff_ast_infix_rhs_let_in() {
    assert_asts_match("let f a x = a && let y = x in y\n");
}

/// `||` operand — the same production fires regardless of which infix operator
/// supplies the RHS (`Token::BarBar` here).
#[test]
fn diff_ast_infix_rhs_let_in_or() {
    assert_asts_match("let f a x = a || let y = x in y\n");
}

/// The `let … in` body greedily absorbs a trailing infix: FCS parses
/// `a && let y = x in y && z` as `a && (let y = x in (y && z))` (the body is a
/// full `declExpr`), **not** `(a && let y = x in y) && z`.
#[test]
fn diff_ast_infix_rhs_let_in_body_absorbs_infix() {
    assert_asts_match("let f a x z = a && let y = x in y && z\n");
}

/// Offside-body form (no explicit `in`): `a &&` ends a line, the `let` is on the
/// next line, and the body follows aligned with the `let` (the corpus
/// `AttributeChecking` shape). LexFilter surfaces no `Raw(In)`; the body follows
/// directly after the binding-RHS `BlockEnd`. (FCS rejects the variant where the
/// body is indented *past* the `let` — that stays a clean error on both sides.)
#[test]
fn diff_ast_infix_rhs_let_offside_body() {
    assert_asts_match("let f a x =\n    a &&\n    let y = x\n    y\n");
}

/// A `let … in` as a *tuple element* — `1, let y = x in y` is
/// `Tuple([1, LetOrUse([y = x], y)])`. Tuple elements descend through
/// `parse_minus_expr`, so the raw-keyword dispatch covers them too.
#[test]
fn diff_ast_tuple_element_let_in() {
    assert_asts_match("let f x = 1, let y = x in y\n");
}

/// A non-block `let … in` inside parentheses but *after* an operator —
/// `(a && let y = x in y)`. The `let` here is still raw (offside promotion only
/// fires at the block head, which the `(`-leading `a` already claimed), so this
/// exercises the raw production with a swallowed-`)` body close.
#[test]
fn diff_ast_paren_infix_rhs_let_in() {
    assert_asts_match("let f a x = (a && let y = x in y)\n");
}

// --- Explicit type-parameter declarations on a binding head -----------------
// FCS reaches these via `headBindingPattern → … opt_explicitValTyparDecls`
// (`pars.fsy`): a `<…>` after the binding name lands in `SynPat.LongIdent`'s
// `typars: SynValTyparDecls option` slot (field 2). A name carrying explicit
// typars is *always* `SynPat.LongIdent` (function form), even with zero curried
// args — `let h<'a> = 3` is a `LongIdent` with empty `args`, not a `Named`.

/// The motivating case — a generic identity function `let identity<'a> (x: 'a)
/// : 'a = x`. The `<'a>` promotes the head to `SynPat.LongIdent` carrying the
/// typar decls, then the single typed paren arg `(x: 'a)` and the `: 'a`
/// `SynBindingReturnInfo` follow. FCS: `SynPat.LongIdent(["identity"], None,
/// Some(SynValTyparDecls(Some(PostfixList[SynTyparDecl 'a]), false)),
/// Pats[Paren(Typed(Named "x", 'a))], None, _)`.
#[test]
fn diff_ast_let_generic_identity() {
    assert_asts_match("let identity<'a> (x: 'a) : 'a = x\n");
}

/// Multiple typars — `let g<'a, 'b> (x: 'a) = x`. Confirms the comma-separated
/// `typarDeclList` (both decls land in `typars`) and that the following curried
/// arg still parses.
#[test]
fn diff_ast_let_generic_two_typars() {
    assert_asts_match("let g<'a, 'b> (x: 'a) = x\n");
}

/// Value-form head with explicit typars and **no** curried args — `let h<'a> =
/// 3`. FCS promotes this to `SynPat.LongIdent(["h"], …, typars=Some …,
/// Pats[], …)` (an empty arg list), *not* a `SynPat.Named`, so the head must
/// take the long-ident branch on the strength of the `<` alone.
#[test]
fn diff_ast_let_generic_value_form() {
    assert_asts_match("let h<'a> = 3\n");
}

/// Typars compose with `inline` — `let inline f<'a> (x: 'a) = x`. The modifier
/// rides on the binding (`SynBinding`/`SynLeadingKeyword`) while the typars ride
/// on the head pattern, distinct slots.
#[test]
fn diff_ast_let_inline_generic() {
    assert_asts_match("let inline f<'a> (x: 'a) = x\n");
}

/// A head-type typar (`^a`) on a binding head — `let f<^a> (x: ^a) = x`.
/// `SynTypar`'s `staticReq` is `HeadType` (pinned by `NormalisedTypar`), so the
/// `^`-vs-`'` distinction must round-trip on both sides.
#[test]
fn diff_ast_let_generic_head_type_typar() {
    assert_asts_match("let f<^a> (x: ^a) = x\n");
}

/// The *spaced* form `let f <'a> (x: 'a) = x` — a non-adjacent `<` with no
/// `HighPrecedenceTyApp` virtual, just a bare raw `Less`. FCS accepts it (with a
/// "non-adjacent type parameters" warning, which the AST diff ignores) and
/// produces the *same* `SynPat.LongIdent` typars shape as the adjacent form, so
/// the head detection must fire on the raw `Less` alone.
#[test]
fn diff_ast_let_generic_spaced_typars() {
    assert_asts_match("let f <'a> (x: 'a) = x\n");
}

/// An **empty** explicit value-typar list — `let f< > (x: int) = x`. A value
/// binding's `explicitValTyparDeclsCore` permits this (FCS accepts it with no
/// diagnostics, producing `Some(SynValTyparDecls(Some(PostfixList [])))`),
/// unlike a *type definition*'s `postfixTyparDecls` where `T<>` is an error.
/// The head must parse the empty `< >` into a (typar-less) `TYPAR_DECLS` node
/// without the spurious "expected a type parameter" diagnostic — both sides
/// project an empty typar list, so the diff matches. (Regression guard for the
/// `permit_empty` flag threaded into `parse_typar_decls_postfix`.)
#[test]
fn diff_ast_let_generic_empty_typars() {
    assert_asts_match("let f< > (x: int) = x\n");
}

/// An attribute on an explicit value-typar declaration — `let f<[<Measure>] 'a>
/// (x: 'a) = x`. The same `SynTyparDecl(attributes, …)` carrier as a type
/// header, reached through the binding head's `explicitValTyparDecls`.
#[test]
fn diff_ast_let_generic_attributed_typar() {
    assert_asts_match("let f<[<Measure>] 'a> (x: 'a) = x\n");
}

/// An arg-bearing attribute on a value-typar declaration —
/// `let f<[<System.CLSCompliant(true)>] 'T> (x: 'T) = x` (the corpus
/// `CustomAttributeGenericParameter01` shape). Exercises the attribute's
/// argument expression inside the typar-decl position.
#[test]
fn diff_ast_let_generic_attributed_typar_with_arg() {
    assert_asts_match("let f<[<System.CLSCompliant(true)>] 'T> (x: 'T) = x\n");
}

/// Named-field group as a `let`-binding head argument — `let f (a = 1) = 2`.
/// The same `atomicPatsOrNamePatPairs → LPAREN namePatPairs rparen` machinery
/// fires for a function-form binding head as for a `match`-clause union-case
/// pattern, so the binding head projects to `SynPat.LongIdent(["f"], …,
/// SynArgPats.NamePatPairs([NamePatPairField(["a"], =, Const 1)]))`.
#[test]
fn diff_ast_let_head_name_pat_pairs() {
    assert_asts_match("let f (a = 1) = 2\n");
}

/// An offside `let`-body sequence whose later statement is a bare negative
/// literal. The adjacent `-` is an `ADJACENT_PREFIX_OP` term-starter, so the
/// body is a `SynExpr.Sequential` of two statements — not a single application
/// of the first statement to `-1`. (Same offside rule as the `try`-body case;
/// covers the general, non-`try` shape.)
#[test]
fn diff_ast_let_body_seq_negative_literal_tail() {
    assert_asts_match("let f x =\n    g x\n    -1\n");
}

/// A bare `: T` annotation on a `let` RHS is `SynExpr.Typed` — FCS's
/// `typedSequentialExpr: sequentialExpr COLON typ` applies at the block-RHS
/// position, not only inside a typed paren. (Previously rejected; see the
/// `bare_colon_is_a_type_annotation` unit test.)
#[test]
fn diff_ast_let_rhs_type_annotation() {
    assert_asts_match("let foo = a : int\n");
}

// ---- Optional-value patterns (`?x`) — `SynPat.OptionalVal` ----

/// A bare optional-value pattern as a curried function-form argument —
/// `let f ?x = x`. FCS's `atomicPattern: QMARK ident` (`pars.fsy:3802`)
/// produces `SynPat.OptionalVal(Ident "x", _)`, so the binding head is
/// `SynPat.LongIdent(["f"], …, SynArgPats.Pats([OptionalVal "x"]))`. The `?`
/// adjacent to the head's argument promotes `f` to the function form exactly as
/// a `Named`/`Wild` argument would. (Optional arguments are only *semantically*
/// valid on type members, but that check is post-parse, so the `ParsedInput`
/// carries the pattern with no error on either side.)
#[test]
fn diff_ast_let_optional_val_arg() {
    assert_asts_match("let f ?x = x\n");
}

/// An optional-value pattern parenthesised as a function-form argument —
/// `let f (?x) = x`. FCS wraps it in `SynPat.Paren(SynPat.OptionalVal("x"))`,
/// the same `Paren` discipline every parenthesised atomic pattern follows.
#[test]
fn diff_ast_let_optional_val_paren_arg() {
    assert_asts_match("let f (?x) = x\n");
}

/// A typed optional-value pattern — `let f (?x: int) = x`. Inside the parens the
/// `: int` attaches to the immediately preceding element, so FCS projects
/// `Paren(Typed(OptionalVal "x", int))` (`parenPattern COLON
/// typeWithTypeConstraints`, `pars.fsy:3929`).
#[test]
fn diff_ast_let_optional_val_typed() {
    assert_asts_match("let f (?x: int) = x\n");
}

/// Two optional-value arguments swept in sequence — `let f ?x ?y = x`. Confirms
/// the curried-arg sweep continues across a `?`-led atomic pattern, projecting
/// `SynArgPats.Pats([OptionalVal "x", OptionalVal "y"])`.
#[test]
fn diff_ast_let_optional_val_two_args() {
    assert_asts_match("let f ?x ?y = x\n");
}

/// An optional-value pattern naming a backtick-quoted identifier —
/// `let f (?``a b``) = x`. FCS strips the backticks in `Ident.idText`, so the
/// `OptionalVal`'s name is `a b`; our `IDENT_TOK`-text projection strips them
/// the same way.
#[test]
fn diff_ast_let_optional_val_quoted_ident() {
    assert_asts_match("let f (?``a b``) = x\n");
}

/// Operator-named binding head, applied form — the reported `let inline
/// (>>>&) (x: int) (y: int) = …`. FCS reduces the parenthesised operator
/// through `opName → pathOp → atomicPatternLongIdent` and, with curried
/// args, lands in `constrPattern: atomicPatternLongIdent
/// atomicPatsOrNamePatPairs` (`pars.fsy:3711`) →
/// `SynPat.LongIdent([op_GreaterGreaterGreaterAmp with
/// OriginalNotationWithParen ">>>&"], None, None,
/// Pats[Paren(Typed(Named x, int)); Paren(Typed(Named y, int))], …)`. Our
/// side emits `LONG_IDENT_PAT > LONG_IDENT > [LPAREN_TOK, IDENT_TOK(">>>&"),
/// RPAREN_TOK]` then the swept paren args; the differential normaliser
/// unwraps FCS's trivia to the same source spelling `">>>&"`.
#[test]
fn diff_ast_let_operator_head_applied() {
    assert_asts_match("let inline (>>>&) (x: int) (y: int) = failwith \"\"\n");
}

/// Operator-named binding head with bare (non-paren) curried args —
/// `let (+) a b = a`. Same `SynPat.LongIdent` reduction as the typed-paren
/// form, exercising the arg sweep over `NAMED_PAT` args.
#[test]
fn diff_ast_let_operator_head_named_args() {
    assert_asts_match("let (+) a b = a\n");
}

/// Operator-named binding head whose operator is the pipe `|>` — confirms a
/// multi-char symbolic op that is *not* an `INFIX_AMP_OP` still rides the
/// general `Token::Op` operator-name path.
#[test]
fn diff_ast_let_operator_head_pipe() {
    assert_asts_match("let (|>) x f = f x\n");
}

/// The range-*step* operator head with curried args — `let (.. ..) v1 v2 = v1`
/// (the `productioncoverage01.fs` shape). `(.. ..)` is FCS's two-token
/// `operatorName: DOT_DOT DOT_DOT` (`op_RangeStep`), the one paren operator name
/// spanning two filtered tokens; the recognition, the operator-value consumption
/// (a `RANGE_STEP_OP` node wrapping the two `..`), and the curried-args lookahead
/// past the `)` all step over both `..`. Reduces to the applied
/// `SynPat.LongIdent([op_RangeStep], …, Pats[Named v1; Named v2])`.
#[test]
fn diff_ast_let_range_step_operator_head_applied() {
    assert_asts_match("let (.. ..) v1 v2 = v1\n");
}

/// The range-step operator head, nullary — `let (.. ..) = id`. With no args it
/// stays the value-form `SynPat.Named(op_RangeStep, ".. ..")`, exercising the
/// two-token name on the nullary (`NAMED_PAT`) path (`NamedPat::range_step_op`
/// surfaces the node).
#[test]
fn diff_ast_let_range_step_operator_head_nullary() {
    assert_asts_match("let (.. ..) = id\n");
}

/// The range-step operator head with the dots *glued* and curried args —
/// `let (....) v1 v2 = v1`. Guards that the applied (`LONG_IDENT_PAT`) path's
/// `RANGE_STEP_OP` node canonicalises to `.. ..` regardless of the inter-dot
/// layout, exactly as the spaced form does.
#[test]
fn diff_ast_let_range_step_operator_head_glued() {
    assert_asts_match("let (....) v1 v2 = v1\n");
}

/// The range-step operator head with a comment between the dots —
/// `let (.. (*c*) ..) a b = a`. The comment stays a trivia token inside the
/// `RANGE_STEP_OP` node; FCS still reduces the head to `op_RangeStep`. Regression
/// guard that the node-keyed canonicalisation is layout- and comment-independent.
#[test]
fn diff_ast_let_range_step_operator_head_inner_comment() {
    assert_asts_match("let (.. (*c*) ..) a b = a\n");
}

/// A backtick-quoted identifier spelled `` ``....`` `` — `let ``....`` = 1`. Its
/// name strips to `....`, which FCS keeps verbatim (`idText = "...."`). This is an
/// ordinary `IDENT_TOK`, *not* a range-step `RANGE_STEP_OP` node, so the
/// normaliser must leave it `....` and never rewrite it to `.. ..` — the
/// regression guard that operator canonicalisation keys on the node, not the
/// dequoted spelling.
#[test]
fn diff_ast_let_quoted_four_dots_ident_not_range_step() {
    assert_asts_match("let ``....`` = 1\n");
}

/// Nullary operator-named binding head — `let (+) = id`. With no curried
/// args FCS reduces through `atomicPattern: atomicPatternLongIdent` to
/// `SynPat.Named(SynIdent("op_Addition", OriginalNotationWithParen "+"), …)`
/// (the singleton-lowercase branch), *not* `SynPat.LongIdent`. Our side
/// emits `NAMED_PAT > [LPAREN_TOK, IDENT_TOK("+"), RPAREN_TOK]`; the
/// normaliser compares the source spelling on both sides.
#[test]
fn diff_ast_let_operator_head_nullary() {
    assert_asts_match("let (+) = id\n");
}

/// A nullary operator name wrapped in an *outer* paren — `let ((+)) = id`.
/// FCS keeps the outer `SynPat.Paren` and the inner `SynPat.Named(op_Addition,
/// "+")`. Confirms the operator-name atomic pattern composes under
/// `parse_paren_pat` (the inner `(op)` is reached as a paren *element* via
/// `emit_paren_pat_element`, not the bare binding head), and that the args
/// lookahead's raw stream correctly declines the swallowed enclosing `)`.
#[test]
fn diff_ast_let_operator_head_paren_wrapped() {
    assert_asts_match("let ((+)) = id\n");
}

/// The raw-stream half of the args lookahead — `let f ((+)) = id`. The inner
/// `(+)` is an *argument* of `f`, wrapped in a paren whose close is
/// LexFilter-swallowed. A filtered-only "args follow?" probe would look past
/// both swallowed `)`s and mis-promote the inner `(+)` to an applied head; the
/// raw lookahead surfaces the enclosing `)` and keeps it the nullary
/// `Paren(Named "+")` argument FCS produces.
#[test]
fn diff_ast_let_operator_arg_paren_wrapped() {
    assert_asts_match("let f ((+)) = id\n");
}

/// The glued `(*)` multiply operator head, applied — `let (*) a b = a`. The
/// lexer emits the dedicated `Token::LParenStarRParen` (not `LParen` + `*`),
/// FCS's `opName: LPAREN_STAR_RPAREN`; with curried args it reduces to
/// `SynPat.LongIdent([op_Multiply with OriginalNotationWithParen "*"], …,
/// Pats[Named a; Named b])`. Our side reuses the expression-side
/// `consume_star_op_value` (`[LPAREN_TOK, IDENT_TOK("*"), RPAREN_TOK]`).
#[test]
fn diff_ast_let_star_operator_head_applied() {
    assert_asts_match("let (*) a b = a\n");
}

/// The glued `(*)` multiply operator head, nullary — `let (*) = id`. With no
/// args it stays a value-form `SynPat.Named(op_Multiply, "*")`. Confirms the
/// star token's nullary path (which can't fall through to `try_emit_atomic_pat`
/// — the star token is not an `is_atomic_pat_start`).
#[test]
fn diff_ast_let_star_operator_head_nullary() {
    assert_asts_match("let (*) = id\n");
}

/// A sign-folded numeric literal as the first argument of an operator head —
/// `let (+) -1 = 0`. `sign_fold` rewrites the filtered `-1` to a single folded
/// `Int32` literal, but the raw stream still shows `Op("-")`; the args
/// lookahead must accept the fold (as the ident-head promotion does) so the
/// head stays the applied `SynPat.LongIdent([op_Addition], …, Pats[Const -1])`
/// rather than a nullary `(+)` with a stray literal.
#[test]
fn diff_ast_let_operator_head_folded_sign_arg() {
    assert_asts_match("let (+) -1 = 0\n");
}

/// An access modifier before an operator head — `let private (+) x y = x`.
/// FCS's `atomicPatternLongIdent: access pathOp` puts `private` before the
/// `opName` `pathOp` (accessibility elided by the normaliser), reducing to the
/// same applied `SynPat.LongIdent([op_Addition], …, Pats[Named x; Named y])` as
/// the unqualified head. Confirms the access gate now recognises an operator
/// head after the modifier (we consume the `ACCESS_TOK` sibling and carry on).
#[test]
fn diff_ast_let_operator_head_access_modifier() {
    assert_asts_match("let private (+) x y = x\n");
}

/// Explicit value-typar declarations on an operator head — `let inline (!!)<'T>
/// (x: 'T) = x`. FCS's `constrPattern: atomicPatternLongIdent
/// explicitValTyparDecls atomicPatsOrNamePatPairs` carries the typars in
/// `SynPat.LongIdent.typars` (field 2) for an operator name exactly as for an
/// ident head, so the operator path parses the `<'T>` between the head and the
/// args. Probes the `TYPAR_DECLS` child of the operator `LONG_IDENT_PAT`.
#[test]
fn diff_ast_let_operator_head_typars() {
    assert_asts_match("let inline (!!)<'T> (x: 'T) = x\n");
}

/// The *spaced* multiply operator head — `let ( * ) a b = a`. FCS's pattern
/// `operatorName` includes the bare `STAR`, and pattern position has no
/// `IndexRange` whole-dimension wildcard for `( * )` to collide with (unlike an
/// expression), so it is the multiply operator `op_Multiply`, parsing the same
/// applied `SynPat.LongIdent` as the glued `(*)`. Exercises the
/// `at_paren_op_value_pat` star admission.
#[test]
fn diff_ast_let_spaced_star_operator_head() {
    assert_asts_match("let ( * ) a b = a\n");
}

/// The glued `(*)` multiply operator-value as a curried *argument* pattern —
/// `let f (*) = 0`. The lexer fuses `(*)` into one `Token::LParenStarRParen`
/// (it would otherwise open a block comment), so the args sweep / atomic
/// dispatch must name it explicitly; FCS reduces it to the `SynPat.Named(
/// op_Multiply, "*")` arg of the `f` head. Exercises `raw_starts_atomic_pat`'s
/// star admission and the matching `try_emit_atomic_pat` arm.
#[test]
fn diff_ast_let_glued_star_operator_arg() {
    assert_asts_match("let f (*) = 0\n");
}

/// The glued `(*)` operator-value as the argument of an *operator* head —
/// `let (+) (*) = 0`. Both the head and the arg are operator names; confirms
/// the operator-head args lookahead (`op_head_args_follow`) admits the glued
/// star token and the sweep consumes it, yielding `SynPat.LongIdent([op_Addition],
/// …, Pats[Named "*"])` rather than a nullary `(+)` plus a stray `(*)`.
#[test]
fn diff_ast_let_operator_head_glued_star_arg() {
    assert_asts_match("let (+) (*) = 0\n");
}

/// `(?)` is the dynamic-lookup operator `op_Dynamic` (FCS's `opName: QMARK`),
/// *not* a malformed optional-value pattern (`?x` needs an ident). As a curried
/// argument — `let f (?) = x` — FCS reduces it to the `SynPat.Named(op_Dynamic,
/// "?")` arg of `f`, with no diagnostics. `?` is in `is_paren_operator_name`, so
/// the `at_paren_op_value_pat` arg path admits `(?)` like any other `( op )`.
#[test]
fn diff_ast_let_dynamic_operator_arg() {
    assert_asts_match("let f (?) = x\n");
}

/// The `op_Dynamic` operator value `(?)` as an argument keeps a following
/// argument separate — `let f (?) y = y` → `SynPat.LongIdent(["f"], …,
/// Pats[Named "?"; Named "y"])`. Confirms `consume_paren_op_value` consumes the
/// swallowed `)` so `y` survives as the second curried arg (the FCS-faithful
/// counterpart of the `paren_question_operator_arg_keeps_next_arg` unit test).
#[test]
fn diff_ast_let_dynamic_operator_arg_keeps_next() {
    assert_asts_match("let f (?) y = y\n");
}

/// The `op_Dynamic` operator as a *binding head* — `let (?) x y = x` defines the
/// dynamic-lookup operator `?`. FCS reduces `(?)` through `opName → pathOp` to
/// `SynPat.LongIdent([op_Dynamic], …, Pats[Named x; Named y])`. This is the
/// canonical reason `?` must stay an operator name in pattern position: excluding
/// it would reject this valid definition.
#[test]
fn diff_ast_let_dynamic_operator_head() {
    assert_asts_match("let (?) x y = x\n");
}

// ---- Funky operator-name binding heads (`.()`, `.()<-`, `.[]`) ------------
//
// FCS's `operatorName: FUNKY_OPERATOR_NAME` (`pars.fsy:6890`) admits the fused
// index-operator names, but reports `deprecatedOperator` (FS0035, a *parse*
// error) for every form except `.[]`, `.()`, `.()<-`. So exactly those three
// parse cleanly; the comma/slice/`.[]<-` forms stay parse errors on both sides
// (they land in `both_reject`). Our lexer fuses each into one
// `Token::FunkyOpName`; the binding-head machinery bumps it as the `IDENT_TOK`
// operator spelling (`.()` / `.[]`), and the differential normaliser unwraps
// FCS's mangled `op_ArrayLookup` / `op_DotLBrackRBrack` + `OriginalNotationWithParen`
// back to that same source spelling.

/// The index-get operator as a binding head — `let (.()) v i = v` (FCS's
/// `op_ArrayLookup`). Applied (curried args), so `SynPat.LongIdent`.
#[test]
fn diff_ast_let_funky_index_get_head() {
    assert_asts_match("let (.()) v i = v\n");
}

/// The index-set operator — `let (.()<-) a b c = ()` (FCS's `op_ArrayAssign`).
/// The `<-` is part of the single fused `FunkyOpName` token.
#[test]
fn diff_ast_let_funky_index_set_head() {
    assert_asts_match("let (.()<-) a b c = ()\n");
}

/// The dot-bracket index operator — `let (.[]) v i = v` (FCS's
/// `op_DotLBrackRBrack`).
#[test]
fn diff_ast_let_funky_dot_bracket_head() {
    assert_asts_match("let (.[]) v i = v\n");
}

/// A nullary funky operator name — `let (.()) = id`. With no curried args FCS
/// reduces to the value-form `SynPat.Named`, exercising the funky-op path of
/// `emit_operator_head` that emits `NAMED_PAT` rather than `LONG_IDENT_PAT`.
#[test]
fn diff_ast_let_funky_index_get_nullary() {
    assert_asts_match("let (.()) = id\n");
}

// ---- Phase 11 error recovery: incomplete `let` binding RHS ----------------
//
// The canonical mid-edit states an LSP/agent buffer hits: a `let` whose RHS is
// missing or unparseable, followed by a *good* declaration. FCS recovers the
// broken binding as `SynBinding(… , SynExpr.ArbitraryAfterError, …)` and keeps
// parsing the next decl; our parser recovers the same way (a `LET_DECL` whose
// `BINDING` has no `Expr` child — a zero-width `ERROR` — then continues). Both
// the recovery placeholder and FCS's `ArbitraryAfterError` normalise to
// `NormalisedExpr::Error`, so the surrounding structure can be diffed. Each uses
// `assert_asts_match_allow_errors` (both sides report ≥1 error and agree on the
// recovered tree). The key property: the trailing `let y = 2` survives — one
// syntax error does not collapse the rest of the file.

/// Nothing after the `=` — `let x =` then a fresh decl. FCS: `Let(x,
/// ArbitraryAfterError)` then `Let(y, Const 2)`; ours matches.
#[test]
fn diff_ast_let_recover_missing_rhs() {
    assert_asts_match_allow_errors("let x =\nlet y = 2\n");
}

/// No `=` at all — `let x` then a fresh decl. FCS still recovers the bodyless
/// binding and continues; ours matches.
#[test]
fn diff_ast_let_recover_missing_eq() {
    assert_asts_match_allow_errors("let x\nlet y = 2\n");
}

/// An invalid token where the RHS should be — `let x = =`. The stray `=` cannot
/// begin an expression, so both sides recover the RHS as the error placeholder
/// and parse the following `let y` as its own decl.
#[test]
fn diff_ast_let_recover_garbage_rhs() {
    assert_asts_match_allow_errors("let x = =\nlet y = 2\n");
}

/// A return-annotated incomplete binding — `let x : int =`. FCS wraps even the
/// recovered RHS in `SynExpr.Typed`, so the recovery placeholder must carry the
/// annotation too (`Typed(Error, int)`, not bare `Error`) to match. Guards the
/// return-type wrapper against being skipped on the recovery path.
#[test]
fn diff_ast_let_recover_missing_rhs_annotated() {
    assert_asts_match_allow_errors("let x : int =\nlet y = 2\n");
}

/// A block `let … in` (`SynExpr.LetOrUse`) whose body is missing — `let x = let
/// z =`. The inner binding's RHS *and* the block body are both recovery holes;
/// FCS fills the body with `SynExpr.ArbitraryAfterError`, which projects to
/// `NormalisedExpr::Error` alongside the (already-handled) binding-RHS hole. The
/// trailing `let y = 2` survives as its own decl.
#[test]
fn diff_ast_letin_recover_missing_body() {
    assert_asts_match_allow_errors("let x = let z =\nlet y = 2\n");
}

/// As above but with an explicit `in` and a present inner RHS — `let x = let z =
/// 1 in`. Only the block body is the hole; it recovers to `Error` and the
/// following decl still parses.
#[test]
fn diff_ast_letin_recover_missing_body_explicit_in() {
    assert_asts_match_allow_errors("let x = let z = 1 in\nlet y = 2\n");
}

// ---------------------------------------------------------------------------
// Module/class-scope `let … in` (the "unsupported token In" divergence class).
//
// A module- or class-scope `let x = e in body` is layout-driven in FCS:
//  - body directly after the `in` (single line, or an indented continuation) →
//    `SynModuleDecl.Expr(SynExpr.LetOrUse)` — the `let … in` is an *expression*.
//  - `in` then a dedent to a sibling decl (`let a = 0 in⏎let b = 1 in⏎()`) →
//    flat `[SynModuleDecl.Let; SynModuleDecl.Let; SynModuleDecl.Expr]`; the `in`
//    is a bare declaration terminator (FCS records `InKeyword = None`).
// LexFilter rewrites the `in` to an `OffsideDeclEnd`, so the distinguisher is
// whether a `Virtual::BlockSep` follows that decl-end (flat) or the body does
// (expression).
// ---------------------------------------------------------------------------

/// Single-line module `let … in` → `SynModuleDecl.Expr(SynExpr.LetOrUse)` with a
/// unit body. The canonical let-in-expression case: the body (`()`) follows the
/// `in` directly, so the whole declaration is one module-level expression, *not*
/// a `SynModuleDecl.Let`.
#[test]
fn diff_ast_module_let_in_unit_body() {
    assert_asts_match("let a = 0 in ()\n");
}

/// Module `let … in` whose body is an infix application — `let a = 0 in a + 1`.
/// Confirms the body goes through the full expression parser (Pratt climb), not
/// just an atom, when reached via the module-scope let-in dispatch.
#[test]
fn diff_ast_module_let_in_app_body() {
    assert_asts_match("let a = 0 in a + 1\n");
}

/// Nested single-line module `let … in let … in body` →
/// `Expr(LetOrUse(a, LetOrUse(b, a + b)))`. The body of the outer let-in is
/// itself a let-in expression, so the module decl nests rather than flattening.
#[test]
fn diff_ast_module_let_in_nested() {
    assert_asts_match("let a = 0 in let b = 1 in a + b\n");
}

/// `let rec … in` at module scope — `let rec f x = x in f 0`. The `rec` flows
/// into the `SynLetOrUse` (`IsRecursive = true`) and the body still follows the
/// `in`.
#[test]
fn diff_ast_module_let_rec_in() {
    assert_asts_match("let rec f x = x in f 0\n");
}

/// Decl-flat module let-in — `let a = 0 in⏎let b = 1 in⏎()`. Each `in` is
/// followed by a dedent to the next sibling declaration, so FCS keeps three flat
/// module declarations (`[Let; Let; Expr]`) and drops each `in`. Our parser must
/// consume the swallowed `in` cleanly rather than stranding it as an
/// "unsupported token In" error.
#[test]
fn diff_ast_module_let_in_decl_flat() {
    assert_asts_match("let a = 0 in\nlet b = 1 in\n()\n");
}

/// A single decl-flat let-in followed by a dedented expression —
/// `let a = 0 in⏎()`. The `in` sits at the end of the binding line and the body
/// dedents to column 0, so this is `[Let; Expr]` (flat), contrasting with the
/// single-line `let a = 0 in ()` (one `Expr(LetOrUse)`).
#[test]
fn diff_ast_module_let_in_decl_flat_single() {
    assert_asts_match("let a = 0 in\n()\n");
}

/// Class-local decl-flat let-in — `type Foo() =⏎ let a = 0 in⏎ member …`. The
/// `let a = 0 in` is a class-local binding whose `in` terminates the declaration
/// before the following member; FCS keeps `[ImplicitCtor; LetBindings; Member]`
/// with the `in` dropped. Exercises the class-body let dispatch
/// (`MEMBER_LET_BINDINGS`) rather than the module one.
#[test]
fn diff_ast_class_local_let_in_decl_flat() {
    assert_asts_match("type Foo() =\n    let a = 0 in\n    member this.A = a\n");
}

/// Decl-flat let-in as the *last* declaration of a nested module —
/// `module M =⏎    let a = 0 in`. The token after the `in`'s decl-end is the
/// enclosing module's `Virtual::BlockEnd` (a scope closer), not a `BlockSep` or
/// a body, so FCS keeps a single flat `SynModuleDecl.Let` (no body, `InKeyword`
/// dropped). The dispatch must treat a scope closer as decl-flat — not mistake
/// the `BlockEnd` for a `let … in` body.
#[test]
fn diff_ast_module_let_in_last_decl_nested() {
    assert_asts_match("module M =\n    let a = 0 in\n");
}

/// Decl-flat let-in as the last declaration of a top-level module —
/// `module M⏎let a = 0 in`. Same scope-closer classification as the nested form
/// but with the module body at column 0; the `in`'s decl-end is followed by the
/// file-closing `BlockEnd`, so it stays a flat `SynModuleDecl.Let`.
#[test]
fn diff_ast_module_let_in_last_decl_toplevel() {
    assert_asts_match("module M\nlet a = 0 in\n");
}

/// Decl-flat let-in as the last declaration of a verbose `begin … end` module
/// body — `module M = begin⏎    let a = 0 in⏎end`. The `in`'s decl-end is
/// followed by the raw `end` keyword closing the block (another scope closer),
/// so FCS keeps a flat `SynModuleDecl.Let`.
#[test]
fn diff_ast_module_let_in_last_decl_begin_end() {
    assert_asts_match("module M = begin\n    let a = 0 in\nend\n");
}

/// Module let-in whose body is a `do` statement — `let a = 0 in do ()`. `do e`
/// is a `declExpr`-level expression starter (`SynExpr.Do`) that surfaces as a
/// `Virtual::Do`, *not* a minus-level / infix-RHS starter. The body-follows gate
/// must use the full expression-start set, so this stays one
/// `SynModuleDecl.Expr(LetOrUse(a, Do ()))` rather than a flat `Let` + a
/// separate `do` decl.
#[test]
fn diff_ast_module_let_in_do_body() {
    assert_asts_match("let a = 0 in do ()\n");
}

/// Module let-in whose body is a leading-`..` range — `let a = 0 in ..3`. A
/// leading `..` is a `declExpr`-level starter (`SynExpr.IndexRange`) deliberately
/// excluded from the infix-RHS lookahead; the body-follows gate must still admit
/// it, keeping this a single `SynModuleDecl.Expr(LetOrUse(a, ..3))`.
#[test]
fn diff_ast_module_let_in_range_body() {
    assert_asts_match("let a = 0 in ..3\n");
}

/// Module let-in reached as a *raw* keyword after a same-line separator —
/// `open System; let a = 0 in ()`. The `;` keeps the `let` from being
/// block-leading, so LexFilter surfaces a raw `Token::Let` and leaves the `in`
/// as a raw `Token::In` (an RHS-close `Virtual::BlockEnd` with no `DeclEnd`),
/// unlike the block-leading form's decl-end-backed `in`. A raw `in` is always
/// inline, so FCS keeps `[Open; Expr(LetOrUse(a, ()))]`; the dispatch must
/// recognise the raw-`In` terminator too, not strand it as an unexpected token.
#[test]
fn diff_ast_module_let_in_raw_after_semi() {
    assert_asts_match("open System; let a = 0 in ()\n");
}

/// Attributed module let-in *expression* on one line — `[<A>] let a = 0 in ()`.
/// The `in` makes the `let` an *expression*, and an attribute list cannot attach
/// to a `let`-expression, so FCS floats the attribute into a standalone
/// `SynModuleDecl.Attributes` followed by a separate `SynModuleDecl.Expr(LetOrUse)`
/// — `[Attributes; Expr]`, no parse errors. The attributed dispatch must detach
/// the attributes here rather than force an attributed flat `Let`.
#[test]
fn diff_ast_module_attributed_let_in_expr_one_line() {
    assert_asts_match("[<System.Obsolete>] let a = 0 in ()\n");
}

/// Attributed module let-in expression with the attribute on its own line —
/// `[<A>]⏎let a = 0 in ()`. Same detachment as the one-line form, but the `let`
/// is block-leading (a `Virtual::Let` after a `BlockSep`) and the `in` surfaces
/// as a decl-end rather than a raw `Token::In`; FCS still yields
/// `[Attributes; Expr(LetOrUse)]`.
#[test]
fn diff_ast_module_attributed_let_in_expr_offside() {
    assert_asts_match("[<System.Obsolete>]\nlet a = 0 in ()\n");
}

/// Attributed *flat* module `let` — `[<A>] let a = 0` (no `in`). The counterpart
/// that must NOT detach: the attribute attaches to the binding, so FCS keeps a
/// single `SynModuleDecl.Let` carrying the attribute. Guards the classifier
/// against over-detaching the ordinary attributed-`let` case.
#[test]
fn diff_ast_module_attributed_let_flat_not_detached() {
    assert_asts_match("[<System.Obsolete>] let a = 0\n");
}
