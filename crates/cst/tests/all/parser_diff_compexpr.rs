//! Differential test (`parser::parse` vs FCS): quotations and computation
//! expressions (bare CEs, `yield`/`return`/`yield!`/`return!`, `do!`). Split
//! out of the former monolithic `parser_diff.rs`.

use crate::common::{assert_asts_match, assert_asts_match_allow_errors};
use borzoi_cst::parser::parse;

// ============================================================================
// Phase 10.1 — quotations (`SynExpr.Quote`)
// ============================================================================

/// Typed quotation `<@ 1 @>`. FCS produces
/// `SynExpr.Quote(operator, isRaw = false, SynExpr.Const(Int32 1),
/// isFromQueryExpression = false, range)`; we emit
/// `QUOTE_EXPR > [LQUOTE_TOK, CONST_EXPR, RQUOTE_TOK]` and both sides
/// project to `Quote { is_raw: false, inner: Const(Int32 1) }`.
#[test]
fn diff_ast_quote_typed() {
    assert_asts_match("<@ 1 @>\n");
}

/// Raw (untyped) quotation `<@@ 1 @@>` — the `isRaw = true` variant.
#[test]
fn diff_ast_quote_raw() {
    assert_asts_match("<@@ 1 @@>\n");
}

/// Quotation with an infix body `<@ 1 + 2 @>`. Pins that the inner
/// expression is the full `parse_expr` surface (the `@>` closer does not
/// get folded into the body as an operator/argument).
#[test]
fn diff_ast_quote_infix_body() {
    assert_asts_match("<@ 1 + 2 @>\n");
}

/// Identifier body `<@ x @>` — the inner is `SynExpr.Ident`.
#[test]
fn diff_ast_quote_ident_body() {
    assert_asts_match("<@ x @>\n");
}

/// Nested typed quotation `<@ <@ 1 @> @>`. The inner quotation is the
/// body of the outer one.
#[test]
fn diff_ast_quote_nested() {
    assert_asts_match("<@ <@ 1 @> @>\n");
}

/// Delimiter mismatch `<@ 1 @@>` (typed opener, raw closer). FCS reports
/// `parsMismatchedQuote` but still builds `SynExpr.Quote` using the
/// *opener*'s `isRaw` (= false), so the AST matches a typed quote; we do
/// the same and emit a parse error on the mismatch.
#[test]
fn diff_ast_quote_mismatch() {
    assert_asts_match_allow_errors("<@ 1 @@>\n");
}

/// A `;`-separated *sequential* quotation body `<@ x; y @>`. FCS's `quoteExpr`
/// body is a `typedSequentialExpr` (`pars.fsy:5434`), so the body is the full
/// statement-sequence surface, not a single expression — `Quote(Sequential(x, y))`.
#[test]
fn diff_ast_quote_sequential_body() {
    assert_asts_match("<@ x; y @>\n");
}

/// The raw-quotation counterpart `<@@ x; y @@>`.
#[test]
fn diff_ast_quote_raw_sequential_body() {
    assert_asts_match("<@@ x; y @@>\n");
}

/// Three statements `<@ x; y; z @>` — nested `Sequential` left-associated.
#[test]
fn diff_ast_quote_sequential_body_three() {
    assert_asts_match("<@ x; y; z @>\n");
}

/// The `TestTP.fs` shape — an assignment then a value, in a raw quotation:
/// `<@@ x <- 1; x @@>` (`Sequential(LongIdentSet/Set, x)`).
#[test]
fn diff_ast_quote_assign_then_value() {
    assert_asts_match("<@@ x <- 1; x @@>\n");
}

/// A trailing type annotation binds the whole body (`typedSequentialExpr:
/// sequentialExpr COLON typ`) — `<@ x : int @>` → `Quote(Typed(x, int))`.
#[test]
fn diff_ast_quote_typed_annotation_body() {
    assert_asts_match("<@ x : int @>\n");
}

/// An offside multi-line sequential body (the separator is the inserted
/// `OBLOCKSEP`, not a literal `;`).
#[test]
fn diff_ast_quote_offside_sequential_body() {
    assert_asts_match("<@\n    x\n    y\n@>\n");
}

// ============================================================================
// Phase 10.2 — bare computation expressions (`SynExpr.ComputationExpr`)
// ============================================================================
//
// Single-expression bodies only. Multi-statement bodies (`seq { 1; 2 }`,
// projecting to a `Sequential` body) need `;`-separated sequential-expression
// support that doesn't exist yet, so they're deferred to a later slice.

/// `seq { 1 }`. FCS produces
/// `App(NonAtomic, false, Ident "seq", ComputationExpr(false, Const 1))`;
/// our `parse_app_expr` juxtaposition wraps the brace `COMPUTATION_EXPR`
/// atom as the argument of `seq`.
#[test]
fn diff_ast_computation_expr_seq_const() {
    assert_asts_match("seq { 1 }\n");
}

/// `seq { x }` — identifier body, to pin that the body is the full
/// expression surface, not just constants.
#[test]
fn diff_ast_computation_expr_seq_ident() {
    assert_asts_match("seq { x }\n");
}

/// `seq { 1 + 2 }` — infix body; the `}` closer is not folded into the
/// body as an operand.
#[test]
fn diff_ast_computation_expr_infix_body() {
    assert_asts_match("seq { 1 + 2 }\n");
}

/// `async { () }` — unit body under a different builder ident.
#[test]
fn diff_ast_computation_expr_async_unit() {
    assert_asts_match("async { () }\n");
}

/// Bare `{ 1 }` (no builder). FCS produces `SynExpr.ComputationExpr`
/// directly (no enclosing `App`); we emit a bare `COMPUTATION_EXPR`.
#[test]
fn diff_ast_computation_expr_bare() {
    assert_asts_match("{ 1 }\n");
}

// Continuation boundary: the `}` is swallowed from the filtered stream, so
// the brace body must stop at it rather than absorbing the following
// infix/app continuation. FCS parses all of these cleanly as
// `(CE …) <continuation>`; without a raw-`RBrace` boundary in the
// continuation guards the body greedily over-reads.

/// `{ 1 } + 2` — bare CE then infix `+`. Must be `(CE 1) + 2`, not
/// `CE(1 + 2)`.
#[test]
fn diff_ast_computation_expr_then_infix() {
    assert_asts_match("{ 1 } + 2\n");
}

/// `seq { 1 } = z` — builder-application CE then infix `=`. Must be
/// `(App(seq, CE 1)) = z`.
#[test]
fn diff_ast_computation_expr_builder_then_infix() {
    assert_asts_match("seq { 1 } = z\n");
}

/// `{ id } 2` — a bare CE applied to an argument. Must be
/// `App(CE id, 2)`, i.e. the CE body is just `id` (it must not absorb
/// the following `2` as an application).
#[test]
fn diff_ast_computation_expr_as_app_func() {
    assert_asts_match("{ id } 2\n");
}

/// `f { 1 } x` — a CE as a middle application argument. The inner CE
/// body must be just `1` (not `1 x`); the whole is `App(App(f, CE 1), x)`.
#[test]
fn diff_ast_computation_expr_as_app_arg() {
    assert_asts_match("f { 1 } x\n");
}

// ============================================================================
// Phase 10.3 — CE control flow: yield / return / yield! / return!
// ============================================================================
//
// `do!` is deferred to the offside/binder slice (10.4): unlike these clean
// keyword-prefix forms, it surfaces as `Virtual::DoBang` + `BlockBegin` /
// `BlockEnd` / `DeclEnd` offside scaffolding.

/// `seq { yield 1 }` → `ComputationExpr(YieldOrReturn((true, false),
/// Const 1))`. `yield` sets the flags tuple to `(isYield = true, false)`.
#[test]
fn diff_ast_ce_yield() {
    assert_asts_match("seq { yield 1 }\n");
}

/// `async { return 1 }` → `YieldOrReturn((false, true), Const 1)`.
/// `return` lexes to the same FCS YIELD token with the bool flipped, so
/// the flags tuple is `(false, true)`.
#[test]
fn diff_ast_ce_return() {
    assert_asts_match("async { return 1 }\n");
}

/// `seq { yield! x }` → `YieldOrReturnFrom((true, false), Ident x)`.
#[test]
fn diff_ast_ce_yield_bang() {
    assert_asts_match("seq { yield! x }\n");
}

/// `async { return! x }` → `YieldOrReturnFrom((false, true), Ident x)`.
#[test]
fn diff_ast_ce_return_bang() {
    assert_asts_match("async { return! x }\n");
}

/// Infix body `seq { yield 1 + 2 }` — the `yield` body is the full
/// expression surface.
#[test]
fn diff_ast_ce_yield_infix_body() {
    assert_asts_match("seq { yield 1 + 2 }\n");
}

/// FCS accepts a top-level `yield 1` (no enclosing CE) and produces
/// `YieldOrReturn` without a parse error, so we match it there too — the
/// keyword forms are `declExpr`-level, not gated to CE context.
#[test]
fn diff_ast_yield_top_level() {
    assert_asts_match("yield 1\n");
}

// ============================================================================
// Phase 10.4 (do! slice) — SynExpr.DoBang
// ============================================================================
//
// `do!` is the offside-virtual member of the binder family: it surfaces as
// `Virtual::DoBang` + `BlockBegin`/`BlockEnd`/`DeclEnd`, parsed with the same
// block machinery as the `if`/`then` body. The rest of 10.4 (let!/use!/and!,
// match!, while!, JoinIn) is blocked — see the plan.

/// `async { do! x }` → `ComputationExpr(DoBang(Ident x))`.
#[test]
fn diff_ast_ce_do_bang() {
    assert_asts_match("async { do! x }\n");
}

/// `async { do! f x }` — application body, to pin that the `do!` body is
/// the full expression surface.
#[test]
fn diff_ast_ce_do_bang_app_body() {
    assert_asts_match("async { do! f x }\n");
}

// ============================================================================
// Phase 10.4b.1 — SynExpr.LetOrUse via the `let!` / `use!` binders
// ============================================================================
//
// `let!`/`use!` surface as `Virtual::Binder` with the same `CtxtLetDecl`
// offside scaffolding as plain `let`; the body is the rest of the enclosing
// SeqBlock. Both project to `SynExpr.LetOrUse` (one `SynLetOrUse`), the
// head binding keyed `LetBang`/`UseBang`. `and!` grouping is 10.4b.2.

/// `async {⏎ let! x = e⏎ return x }` → `ComputationExpr(LetOrUse([LetBang x =
/// e], body=YieldOrReturn x))`.
#[test]
fn diff_ast_ce_let_bang() {
    assert_asts_match("async {\n    let! x = e\n    return x\n}\n");
}

/// `use!` shares the `BINDER`/`OffsideBinder` token with `let!`; the binding's
/// leading keyword is `UseBang`.
#[test]
fn diff_ast_ce_use_bang() {
    assert_asts_match("async {\n    use! x = e\n    return x\n}\n");
}

/// Explicit-`in` form `let! x = e in return x` — same `LetOrUse`, no offside
/// `BlockSep` before the body (the raw `in` is consumed by LexFilter's IN arm).
#[test]
fn diff_ast_ce_let_bang_explicit_in() {
    assert_asts_match("async {\n    let! x = e in return x\n}\n");
}

/// Application RHS — pins that the binding RHS is the full expression surface.
#[test]
fn diff_ast_ce_let_bang_app_rhs() {
    assert_asts_match("async {\n    let! x = f y\n    return x\n}\n");
}

/// Multi-statement body ⇒ `SynLetOrUse.Body` is a `Sequential`.
#[test]
fn diff_ast_ce_let_bang_seq_body() {
    assert_asts_match("async {\n    let! x = a\n    return x\n    return y\n}\n");
}

/// A fresh `let!` after a `let!` (no `and!`) nests: the body of the first
/// `LetOrUse` is a second `LetOrUse`. Falls out of the recursive body parse.
#[test]
fn diff_ast_ce_let_bang_nested() {
    assert_asts_match("async {\n    let! x = a\n    let! y = b\n    return x\n}\n");
}

// ============================================================================
// Phase 10.4b.2 — `and!` applicative grouping
// ============================================================================
//
// `let! … and! …` is one `SynExpr.LetOrUse` with several `Bindings`
// (`IsRecursive = false`), the head keyed `LetBang`, the followers `AndBang`.
// Contrast `diff_ast_ce_let_bang_nested` above, where a fresh `let!` (no `and!`)
// nests instead.

/// `let! x = a⏎ and! y = b⏎ return x` → one `LetOrUse` with two bindings.
#[test]
fn diff_ast_ce_and_bang() {
    assert_asts_match("async {\n    let! x = a\n    and! y = b\n    return x\n}\n");
}

/// Two `and!` followers ⇒ one `LetOrUse` with three bindings.
#[test]
fn diff_ast_ce_and_bang_two() {
    assert_asts_match("async {\n    let! x = a\n    and! y = b\n    and! z = c\n    return x\n}\n");
}

/// `use!` head with an `and!` follower — the head is `UseBang`, the follower
/// `AndBang` (the grouping is independent of the head keyword).
#[test]
fn diff_ast_ce_use_bang_and_bang() {
    assert_asts_match("async {\n    use! x = a\n    and! y = b\n    return x\n}\n");
}

// ============================================================================
// Phase 10.4c — `match!` (`SynExpr.MatchBang`)
// ============================================================================
//
// `match! e with …` is `SynExpr.MatchBang(debugPoint, expr, clauses, range,
// trivia)` (`SyntaxTree.fsi:916`) — field-for-field identical to
// `SynExpr.Match` apart from the case name. `Token::MatchBang` is a raw token
// (LexFilter pushes `CtxtMatch`/`CtxtMatchClauses` but does not relabel it),
// so the filtered stream is `MatchBang expr With <clauses> RightBlockEnd End`
// — a clone of `match`'s. The parser mirrors `parse_match_expr`, reusing the
// shared `parse_match_clauses`. Both sides project to `NormalisedExpr::MatchBang
// { scrutinee, clauses }` (distinct from `Match` to keep the FCS case honest).
// FCS parses `match!` at any expression position (not only inside a CE), so
// the bare let-RHS / top-level forms are valid oracles alongside the CE form.

/// Bare top-level `match! x with A -> 1` — FCS accepts `match!` outside a CE
/// (`ParseHadErrors = false`). The simplest oracle: one clause, no `|`, no
/// `when`.
#[test]
fn diff_ast_match_bang_single_clause() {
    assert_asts_match("match! x with A -> 1\n");
}

/// Inside a computation expression — the canonical `match!` site. The CE body
/// is the single `MatchBang`; the `}` is the swallowed closer.
#[test]
fn diff_ast_match_bang_in_ce() {
    assert_asts_match("async { match! x with A -> 1 }\n");
}

/// Multiple clauses with a leading `|` separator — exercises the shared
/// per-clause `RightBlockEnd` drain through `parse_match_clauses`.
#[test]
fn diff_ast_match_bang_multi_clause() {
    assert_asts_match("match! x with\n| A -> 1\n| B -> 2\n");
}

/// `when` guard on a `match!` clause — the guard is the leading `Expr` child.
#[test]
fn diff_ast_match_bang_when_guard() {
    assert_asts_match("match! x with A when c -> 1\n");
}

/// Ctor-application clause pattern (`Some y`) — the binder `y` scopes the
/// clause result, exercising the shared `parenPattern` head-binding entry.
#[test]
fn diff_ast_match_bang_ctor_clause() {
    assert_asts_match("match! x with Some y -> y\n");
}

/// `match!` as a `let`-binding RHS, scrutinee an application — pins that the
/// scrutinee is the full expression surface and the let-RHS block close
/// survives the clause-list drain (`let f x = match! …` mirrors the
/// `diff_ast_match_as_let_rhs` guard for plain `match`).
#[test]
fn diff_ast_match_bang_as_let_rhs() {
    assert_asts_match("let f x = match! g x with A -> 1\n");
}

/// `do!` with an explicit verbose-syntax `done` terminator
/// (`async { do! f done }`). LexFilter relabels the raw `done` to the do!
/// body's closing `Virtual::DeclEnd` at the `done` span; the parser must claim
/// it as `DONE_TOK` so the swallowed CE `}` still reaches the closer rather than
/// tripping on a leftover `done`. (Shares the fix with `while … do … done`.)
#[test]
fn diff_ast_ce_do_bang_done() {
    assert_asts_match("async { do! f done }\n");
}

// ============================================================================
// Phase 10.4e — `while!` (`SynExpr.WhileBang`)
// ============================================================================
//
// `while! cond do body` is the computation-expression while binder,
// `SynExpr.WhileBang(whileDebugPoint, whileExpr, doExpr, range)`
// (`SyntaxTree.fsi:928`) — identical fields to `SynExpr.While`. `Token::WhileBang`
// is a plain raw token (like `Token::While`), so the filtered stream is a clone
// of plain `while`'s, and the parser routes it through the same parametrised
// `parse_while_loop`. Both sides project to `NormalisedExpr::WhileBang { cond,
// body }` (distinct from `While` to keep the FCS case honest). FCS parses
// `while!` at any expression position, not only inside a CE.

/// Inside a computation expression — the canonical `while!` site.
#[test]
fn diff_ast_while_bang_in_ce() {
    assert_asts_match("async { while! c do () }\n");
}

/// Bare top-level `while! c do ()` — FCS accepts `while!` outside a CE
/// (`ParseHadErrors = false`).
#[test]
fn diff_ast_while_bang_bare() {
    assert_asts_match("while! c do ()\n");
}

/// Multi-statement offside body ⇒ the `doExpr` is a `SynExpr.Sequential`,
/// reusing the same `parse_if_body` body machinery as plain `while`.
#[test]
fn diff_ast_while_bang_multi_statement_body() {
    assert_asts_match("async {\n  while! c do\n    a\n    b\n}\n");
}

/// Explicit `done` terminator (`while! c do f done`) — rides on the shared
/// `consume_block_decl_end` that `while`/`do!` already use.
#[test]
fn diff_ast_while_bang_done_terminator() {
    assert_asts_match("async { while! c do f done }\n");
}

// ============================================================================
// Phase 10 (record expressions) — `SynExpr.Record`
// ============================================================================
//
// `{ … }` is overloaded across record/object/computation expressions
// (`braceExprBody`, `pars.fsy:5580`). Before this slice every `{ … }` was a
// `COMPUTATION_EXPR`; now the brace parser disambiguates via a bounded raw
// lookahead — a leading longident followed by `=` (field assignment) or `with`
// (copy-update) is a `SynExpr.Record(baseInfo=None, copyInfo, recordFields,
// range)` (`SyntaxTree.fsi:634`); everything else stays a computation
// expression. Object expressions (`{ new T … }`) are deferred — they need
// member syntax (phase 9). Both sides project to `NormalisedExpr::Record { copy,
// fields }`; the per-field `equalsRange`/`blockSeparator`/range trivia is elided.

/// Single field `{ F = 1 }` — `Record(None, None, [F = Const 1])`. The leading
/// longident `F` then `=` selects the record production over a CE.
#[test]
fn diff_ast_record_single_field() {
    assert_asts_match("let r = { F = 1 }\n");
}

/// Multiple fields, single-line `;` separators (`{ F = 1; G = 2 }`).
#[test]
fn diff_ast_record_multi_field() {
    assert_asts_match("let r = { F = 1; G = 2 }\n");
}

/// Offside field separators (`Virtual::BlockSep`) instead of explicit `;`.
#[test]
fn diff_ast_record_offside_fields() {
    assert_asts_match("let r =\n  { F = 1\n    G = 2 }\n");
}

/// A *blank line* between offside fields. FCS accepts this (verified
/// `ParseHadErrors: false`); the blank line still produces a single
/// `Virtual::BlockSep`, so it is one `seps` group, not a repeated separator.
/// Pins that tightening the separator loop to one group per gap keeps the
/// blank-line idiom valid.
#[test]
fn diff_ast_record_blank_line_between_fields() {
    assert_asts_match("let r =\n  { F = 1\n\n    G = 2 }\n");
}

/// Qualified field name `{ A.B = 1 }` — the field name is a dotted
/// `SynLongIdent`, so the longident lookahead must span the whole path.
#[test]
fn diff_ast_record_qualified_field() {
    assert_asts_match("let r = { A.B = 1 }\n");
}

/// Copy-and-update `{ x with F = 1 }` — `copyInfo = Some(Ident x)`. The `with`
/// surfaces as `Virtual::With` (backed by a raw `Token::With`) plus a trailing
/// `Virtual::End` that the parser consumes before the swallowed `}`.
#[test]
fn diff_ast_record_copy_update() {
    assert_asts_match("let r = { x with F = 1 }\n");
}

/// Copy-update with several updated fields.
#[test]
fn diff_ast_record_copy_update_multi() {
    assert_asts_match("let r = { x with F = 1; G = 2 }\n");
}

/// A field value that is itself a full expression (application) — pins that the
/// value is `parse_expr`, stopping at the `;`/`}` separator.
#[test]
fn diff_ast_record_field_value_app() {
    assert_asts_match("let r = { F = f x }\n");
}

/// Bare top-level record `{ F = 1 }` (no `let` RHS) — FCS parses it as a
/// module-level `Record` do-expr.
#[test]
fn diff_ast_record_bare_top_level() {
    assert_asts_match("{ F = 1 }\n");
}

/// Disambiguation guard: `{ x }` (a single ident, no `=`/`with`) stays a
/// `ComputationExpr`, not a record. Pins that the record lookahead falls back
/// to a CE when the leading longident is not followed by `=`/`with`.
#[test]
fn diff_ast_record_lookahead_ce_fallback() {
    assert_asts_match("let r = { x }\n");
}

/// A record followed by a sibling statement in the same offside block:
/// `{ F = 1 }⏎ y` ⇒ `Sequential(Record, Ident y)`. The closing `}` is swallowed
/// but still precedes the offside `Virtual::BlockSep` in the raw stream, so the
/// record's field-separator loop must stop at the `}` rather than swallowing
/// the outer separator and mis-parsing `y` as a bogus field.
#[test]
fn diff_ast_record_then_sibling_statement() {
    assert_asts_match("let f () =\n    { F = 1 }\n    y\n");
}

/// Nested record as a field value `{ F = { G = 1 } }` — the inner record's
/// swallowed `}` is claimed by its own `bump_swallowed_closer`, and the outer
/// field-separator loop then stops at the outer `}` (raw-lookahead gate).
#[test]
fn diff_ast_record_nested() {
    assert_asts_match("let r = { F = { G = 1 } }\n");
}

/// Empty braces `{}` are an *empty record* `SynExpr.Record(None, None, [], _)`,
/// not a computation expression (FCS's `LBRACE rbrace` arm). The `}` is
/// swallowed, so the classifier detects the empty brace on the raw stream.
#[test]
fn diff_ast_record_empty() {
    assert_asts_match("let r = {}\n");
}

/// Empty braces with interior whitespace `{ }` — same empty record.
#[test]
fn diff_ast_record_empty_whitespace() {
    assert_asts_match("let r = { }\n");
}

/// Bare top-level empty record `{}`.
#[test]
fn diff_ast_record_empty_bare() {
    assert_asts_match("{}\n");
}

/// `seq {}` is `App(seq, Record [])` — even with a builder prefix, empty braces
/// are an empty record, not an empty computation expression.
#[test]
fn diff_ast_record_empty_with_builder() {
    assert_asts_match("let r = seq {}\n");
}

/// A `global`-rooted qualified field name `{ global.N.R.F = 1 }`. F#'s `global`
/// path head must be accepted by the record disambiguation lookahead (and by
/// `peek_is_record_field_start`), like `parse_long_ident_path` already does;
/// FCS spells the segment `` `global` ``, which the normaliser strips to
/// `global` on both sides.
#[test]
fn diff_ast_record_global_field_path() {
    assert_asts_match("let r = { global.N.R.F = 1 }\n");
}

// Offside field values. FCS's field value is a block-scoped `declExprBlock`, so
// a value on the next line after `=` gets a `Virtual::BlockBegin`/`BlockEnd`
// SeqBlock (and a multi-statement offside value is a `Sequential`) — the same
// scaffolding `let`/`if`/`do!` RHSs use. The field value reuses `parse_if_body`.

/// Single offside field value: `{ F =⏎ 1 }`.
#[test]
fn diff_ast_record_offside_value() {
    assert_asts_match("let r =\n  { F =\n      1 }\n");
}

/// A field whose offside value is multi-statement ⇒ the value is a
/// `SynExpr.Sequential` (`{ F =⏎ a⏎ b }`).
#[test]
fn diff_ast_record_offside_value_sequential() {
    assert_asts_match("let r =\n  { F =\n      a\n      b }\n");
}

/// Mixed: an inline first field then a second field with an offside value
/// (`{ F = 1⏎ G =⏎ 2 }`). Pins that the inter-field separator and the second
/// field's value-block scaffolding compose.
#[test]
fn diff_ast_record_offside_multi_field() {
    assert_asts_match("let r =\n  { F = 1\n    G =\n      2 }\n");
}

/// Copy-update with the updated field offside after `with`
/// (`{ x with⏎ F = 1 }`).
#[test]
fn diff_ast_record_offside_copy_update() {
    assert_asts_match("let r =\n  { x with\n      F = 1 }\n");
}

// ---- copy-update sources that are full `appExpr`s (not a bare longident) ----
// FCS's `recdExprCore: appExpr WITH …` accepts *any* `appExpr` as the copy
// source, not just a longident. The brace classifier therefore can't decide
// record-vs-CE by a bare-longident lookahead (the appExpr's `)`/`}` closers are
// LexFilter-swallowed): the leading expression is parsed for real, then a
// trailing `with` selects the copy-update record.

/// Copy source is a function application `f x` (`{ f x with F = 1 }`) — the
/// `with` follows a two-token appExpr, so the bare-longident lookahead misses it.
#[test]
fn diff_ast_record_copy_update_app_source() {
    assert_asts_match("let r = { f x with F = 1 }\n");
}

/// Copy source is a qualified zero-arg call `Foo.Bar ()` (`{ Foo.Bar () with
/// F = 1 }`) — the `()`'s `)` is swallowed, so the `with` sits directly after
/// the `(` in the filtered stream.
#[test]
fn diff_ast_record_copy_update_app_source_unit_call() {
    assert_asts_match("let r = { Foo.Bar () with F = 1 }\n");
}

/// Copy source is a multi-argument application `f x y` with several updated
/// fields.
#[test]
fn diff_ast_record_copy_update_app_source_multi() {
    assert_asts_match("let r = { f x y with F = 1; G = 2 }\n");
}

/// The reported case: an `appExpr`-source copy-update whose field value is a
/// tuple-pattern lambda, the whole record then upcast with `:> _`
/// (`{ PublicTypeMock.Empty () with Mem1 = fun (s, count) -> … } :> _`).
#[test]
fn diff_ast_record_copy_update_app_source_lambda_coerced() {
    assert_asts_match(
        "let mock : IPublicType =\n    { PublicTypeMock.Empty () with\n        Mem1 = fun (s, count) -> List.replicate count s\n    }\n    :> _\n",
    );
}

/// Guard: a CE body whose leading statement is an application with *no* trailing
/// `with` (`seq { foo (); bar () }`) must still classify as a computation
/// expression, not a record — the appExpr-source path falls back to a CE when no
/// `with` follows.
#[test]
fn diff_ast_record_app_head_no_with_stays_ce() {
    assert_asts_match("let r = seq { foo (); bar () }\n");
}

/// The source's last argument is parenthesised (`{ f (g x) with F = 1 }`): the
/// `(g x)`'s `)` is swallowed, so the `with` must be attributed to *this* brace
/// via the raw-stream nesting guard, not stolen by the inner paren scope.
#[test]
fn diff_ast_record_copy_update_app_source_paren_arg() {
    assert_asts_match("let r = { f (g x) with F = 1 }\n");
}

/// A nested copy-update record as the updated field value of an `appExpr`-source
/// copy-update (`{ f x with F = { y with G = 1 } }`): the inner record's `with`
/// must bind to the inner brace and the outer `with` to the outer brace.
#[test]
fn diff_ast_record_copy_update_app_source_nested_value() {
    assert_asts_match("let r = { f x with F = { y with G = 1 } }\n");
}

/// The `appExpr` source contains a nested *ident-only* brace argument
/// (`{ f { x } with F = 1 }`). The inner `{ x }`'s `}` is LexFilter-swallowed, so
/// without a raw-stream nesting guard its bare-longident classifier would see the
/// outer `with` and steal the update fields; the guard keeps the `with` bound to
/// the outer brace, so this parses as a copy-update of `f { x }`.
#[test]
fn diff_ast_record_copy_update_app_source_nested_ident_brace() {
    assert_asts_match("let r = { f { x } with F = 1 }\n");
}

/// Same nesting hazard one level deeper: a `seq { x }` computation expression as
/// an `appExpr` argument (`{ f (seq { x }) with F = 1 }`) — the inner `{ x }`'s
/// `}` *and* the `)` are swallowed, so the raw-stream guard must still attribute
/// the outer `with` to the outer brace.
#[test]
fn diff_ast_record_copy_update_app_source_nested_seq_arg() {
    assert_asts_match("let r = { f (seq { x }) with F = 1 }\n");
}

/// A computation-expression body whose first statement is an `appExpr` extended
/// by an infix operator (`{ f x + g y }`) — the `appExpr` source parse must
/// resume the precedence climb so the whole `f x + g y` is one CE statement, not
/// stop at `f x`.
#[test]
fn diff_ast_record_app_head_infix_ce() {
    assert_asts_match("let r = seq { f x + g y }\n");
}

/// `global` is accepted as a record field-name path (`{ global.N.F = 1 }`) but
/// is not yet a supported *expression* atom. The appExpr-source classifier must
/// therefore keep a `global`-headed application head off the `parse_app_expr`
/// route — which would otherwise reach `parse_const_payload`'s `unreachable!` —
/// and recover gracefully like the bare-longident path. Pins that these inputs
/// parse without panicking. (Both reach the computation/error path, so they just
/// record diagnostics; the point is *no panic*.)
#[test]
fn global_headed_app_source_does_not_panic() {
    // A `global`-rooted application as a copy-update source.
    let _ = parse("let r = { global.Factory.Create () with F = 1 }\n");
    // A `global`-rooted application as a bare computation-expression body.
    let _ = parse("let y = seq { global.Foo.Bar () }\n");
    // A `global`-rooted application with no trailing `with`.
    let _ = parse("let r = { global.Foo.Bar () }\n");
}

/// Strictness guard: FCS's `recdExprCore` is `appExpr WITH`, so a `with`
/// following a *non-appExpr* source is rejected. `{ a + b with F = 1 }` has an
/// infix source, so FCS reports FS0010 (`Unexpected keyword 'with'`); our parser
/// must flag the same `with` rather than silently accept a copy-update. Pins that
/// the `appExpr`-level source parse does not over-accept.
#[test]
fn record_copy_update_rejects_infix_source() {
    let src = "let r = { a + b with F = 1 }\n";
    let parse = parse(src);
    let with_span = src.find("with").expect("`with` present");
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.span.start == with_span && e.span.end == with_span + "with".len()),
        "expected a parse error at the `with` (FCS rejects `appExpr`-less copy-update sources); \
         got {:?}",
        parse.errors,
    );
}

// --- Copy-update with a non-ident-headed `appExpr` source -------------------
// FCS's `recdExprCore: appExpr WITH …` admits any `appExpr` copy source, not
// only an ident-headed one. The bare-longident classifier routed only
// `Ident`/`global` heads to the appExpr-source path; a source headed by a
// literal, `(`, `[|`/`[`, or the adjacent-prefix deref `!` fell to the
// computation arm and errored at the `with`. These exercise the broadened
// routing.

/// Deref source — `{ !anchor with top = 1 }` (`!` is an adjacent prefix op, an
/// `appExpr` head). The corpus `IlxGen`/event-loop shape.
#[test]
fn diff_ast_record_copy_update_deref_source() {
    assert_asts_match("let r = { !anchor with top = 1 }\n");
}

/// Array-literal source — `{ [| 1 |] with B = 2 }` (an atomic `appExpr` head).
#[test]
fn diff_ast_record_copy_update_array_source() {
    assert_asts_match("let r = { [| 1 |] with B = 2 }\n");
}

/// Parenthesised-then-indexed source — `{ (a).[0] with Age = 0 }` (a paren atom
/// with a `.[ ]` indexer, an `appExpr`).
#[test]
fn diff_ast_record_copy_update_paren_indexed_source() {
    assert_asts_match("let r = { (a).[0] with Age = 0 }\n");
}

/// Parenthesised source — `{ (foo) with X = 1 }`.
#[test]
fn diff_ast_record_copy_update_paren_source() {
    assert_asts_match("let r = { (foo) with X = 1 }\n");
}

/// Indexed application source — `{ (g 1).[0] with X = 1 }` (the swallowed `)` of
/// the inner call plus the `.[ ]` indexer).
#[test]
fn diff_ast_record_copy_update_paren_app_indexed_source() {
    assert_asts_match("let r = { (g 1).[0] with X = 1 }\n");
}

/// Guard: a non-ident atomic head with *no* trailing `with` stays a computation
/// expression — `{ [| 1 |] }` is `ComputationExpr([|1|])`, not a record. The
/// broadened routing must still fall back to a CE when no `with` follows.
#[test]
fn diff_ast_record_array_head_no_with_stays_ce() {
    assert_asts_match("let r = seq { [| 1 |] }\n");
}

/// Strictness guard: a *unary-minus* source is not an `appExpr` (FCS's
/// `minusExpr`), so `{ -x with F = 1 }` is rejected (FS0010 at the `with`) just
/// like the infix source. Pins that the broadened routing doesn't over-accept.
#[test]
fn record_copy_update_rejects_unary_minus_source() {
    let src = "let r = { -x with F = 1 }\n";
    let parse = parse(src);
    let with_span = src.find("with").expect("`with` present");
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.span.start == with_span && e.span.end == with_span + "with".len()),
        "expected a parse error at the `with` (a unary-minus source is not an appExpr); got {:?}",
        parse.errors,
    );
}

// ============================================================================
// Computation-expression bodies with multiple `;`/newline-separated statements
// (`SynExpr.ComputationExpr` wrapping a `SynExpr.Sequential`). Previously only a
// single body expression was parsed, so `seq { yield 1; yield 2 }` errored at
// the `;`.
// ============================================================================

/// Two `;`-separated yields on one line.
#[test]
fn diff_ast_compexpr_seq_two_yields_semi() {
    assert_asts_match("let a = seq { yield 1; yield 2 }\n");
}

/// Two newline-separated yields (offside `BlockSep`).
#[test]
fn diff_ast_compexpr_seq_two_yields_offside() {
    assert_asts_match("let a =\n    seq {\n        yield 1\n        yield 2\n    }\n");
}

/// `yield` then `yield!` (the original `SeqExpressionTailCalls01` shape).
#[test]
fn diff_ast_compexpr_yield_then_yieldbang() {
    assert_asts_match("let rec rwalk x = seq { yield x; yield! rwalk (x + 1) }\n");
}

/// An `async` CE with a statement then `return` (the `MissingIgnore` shape).
#[test]
fn diff_ast_compexpr_async_stmt_then_return() {
    assert_asts_match("let a = async { 1; return 2 }\n");
}

/// Three statements separated by `;`.
#[test]
fn diff_ast_compexpr_three_statements() {
    assert_asts_match("let a = seq { yield 1; yield 2; yield 3 }\n");
}

/// A `let` binding inside a CE body followed by a `yield`.
#[test]
fn diff_ast_compexpr_let_then_yield() {
    assert_asts_match("let a = seq { let x = 1 in yield x }\n");
}

/// `do!` then `return!` in an async CE.
#[test]
fn diff_ast_compexpr_dobang_then_returnbang() {
    assert_asts_match("let a = async { do! x; return! y }\n");
}

/// A *bare* trailing annotation in a CE body (`seq { 1 : int }`) is an FCS
/// error — `computationExpr` is a non-typed `sequentialExpr`, so the `:` must be
/// parenthesised. Both sides must reject it (regression: parsing the body as a
/// `typedSequentialExpr` would silently accept it).
#[test]
fn diff_ast_compexpr_bare_annotation_is_error() {
    assert_asts_match_allow_errors("let a = seq { 1 : int }\n");
}

/// A *parenthesised* annotation inside a CE body (`seq { (1 : int) }`) is fine —
/// the `:` lives inside the paren, not the sequence's trailing position.
#[test]
fn diff_ast_compexpr_paren_annotation_ok() {
    assert_asts_match("let a = seq { (1 : int) }\n");
}

/// A `new`-headed CE body with a following statement (`seq { new T(); yield 1 }`)
/// — the `new T()` base call is the first element of the sequence, gathered via
/// the shared `finish_seq_block`. Previously only the construction was parsed and
/// the `;` errored as a missing `}`.
#[test]
fn diff_ast_compexpr_new_headed_then_yield() {
    assert_asts_match("let a = seq { new System.Object(); yield 1 }\n");
}

/// `async { new T(); return 1 }` — the new-headed sequence in an `async` CE.
#[test]
fn diff_ast_compexpr_new_headed_async_return() {
    assert_asts_match("let a = async { new System.Object(); return 1 }\n");
}

/// A `new`-headed CE with three statements (offside), confirming the gatherer
/// continues past the construction for several elements.
#[test]
fn diff_ast_compexpr_new_headed_offside_seq() {
    assert_asts_match(
        "let a =\n    seq {\n        new System.Object()\n        yield 1\n        yield 2\n    }\n",
    );
}

/// A bare single construction `seq { new T() }` stays a lone computation body
/// (no `SEQUENTIAL_EXPR` wrap) — guards that the gatherer doesn't over-wrap.
#[test]
fn diff_ast_compexpr_new_headed_single() {
    assert_asts_match("let a = seq { new System.Object() }\n");
}

// ============================================================================
// Typed CE binders — `let! x : T = e` / `use! …` / `and! …` (FCS's
// `AllowTypedLetUseAndBang`: the binder's `SynBinding.returnInfo` is set but the
// RHS is **not** wrapped in `SynExpr.Typed`, unlike a regular typed `let`).
// ============================================================================

/// A typed `let!` binder (`let! x : int = e`).
#[test]
fn diff_ast_compexpr_typed_letbang() {
    assert_asts_match("let f = async {\n    let! x : int = g()\n    return x\n}\n");
}

/// A typed `use!` binder (`use! r : IDisposable = e`).
#[test]
fn diff_ast_compexpr_typed_usebang() {
    assert_asts_match("let f = async {\n    use! r : System.IDisposable = g()\n    return 1\n}\n");
}

/// A typed `and!` follower (`let! x : int = a and! y : string = b`).
#[test]
fn diff_ast_compexpr_typed_andbang() {
    assert_asts_match(
        "let f = async {\n    let! x : int = a\n    and! y : string = b\n    return x\n}\n",
    );
}

/// A typed `let!` over a *record* pattern (`let! { Name = name } : Person = e`
/// — the `Typed LetBang 08` shape).
#[test]
fn diff_ast_compexpr_typed_letbang_record_pat() {
    assert_asts_match(
        "let f = async {\n    let! { Name = name } : Person = asyncPerson()\n    return name\n}\n",
    );
}

/// A typed `let!` over a *union* pattern (`let! (Union value) : int option = e`
/// — the `Typed LetBang 11` shape).
#[test]
fn diff_ast_compexpr_typed_letbang_union_pat() {
    assert_asts_match(
        "let f = async {\n    let! (Union value) : int option = asyncOption()\n    return value\n}\n",
    );
}

// ============================================================================
// Phase 10.4 (JoinIn slice) — SynExpr.JoinIn
// ============================================================================
//
// `join x in xs on (a = b)` inside a `query { … }` computation expression.
// The LexFilter rewrites the `in` token to `JOIN_IN` whenever it sits in a
// brace-CE context (`detectJoinInCtxt`, LexFilter.fs:747); the grammar's
// `declExpr JOIN_IN declExpr` (`pars.fsy:4669`) then builds
// `SynExpr.JoinIn(lhsExpr, lhsRange, rhsExpr, range)`. The detection is purely
// contextual — tied to the enclosing brace, not to the `join`/`on` words —
// so even `query { a in b }` is `JoinIn(a, b)`.

/// The canonical form `query { join x in xs on (a = b) }`. FCS produces
/// `App(Ident query, ComputationExpr(JoinIn(App(join, x), App(App(xs, on),
/// Paren(a = b)))))`. The `join x` LHS is a plain application, the
/// `xs on (a = b)` RHS a left-nested application of `on` to the parenthesised
/// equality.
#[test]
fn diff_ast_ce_join_in() {
    assert_asts_match("query { join x in xs on (a = b) }\n");
}

/// A *bare* brace CE `{ join x in xs on (a = b) }` (no `query` builder) — the
/// `JoinIn` is the brace's sole statement, with no enclosing `App`.
#[test]
fn diff_ast_ce_join_in_bare_brace() {
    assert_asts_match("{ join x in xs on (a = b) }\n");
}

/// The minimal shape `query { a in b }`. The detection is contextual, so the
/// bare `in` between two idents is `JoinIn(Ident a, Ident b)` — no `join`/`on`
/// keywords required.
#[test]
fn diff_ast_ce_join_in_minimal() {
    assert_asts_match("query { a in b }\n");
}

/// `query { join x in xs }` — `JoinIn(App(join, x), Ident xs)`, pinning that
/// the RHS is the full application surface but stops at the swallowed `}`.
#[test]
fn diff_ast_ce_join_in_no_on() {
    assert_asts_match("query { join x in xs }\n");
}

/// Non-misfire: a `let … in` *directly inside* a `query` brace stays
/// `SynExpr.LetOrUse` — the `in` balances the `let`'s `CtxtLetDecl` and is
/// **not** rewritten to `JOIN_IN` (the join detection skips only
/// seq-block/`do`/`for` between the `Vanilla` head and the brace; a `LetDecl`
/// blocks the walk). Pins that the new detection doesn't over-fire on the
/// keyword `in` it shares a context with.
#[test]
fn diff_ast_ce_let_in_inside_query_is_not_join() {
    assert_asts_match("query { let x = 1 in x }\n");
}

/// Non-misfire: a `for … in` directly inside a `query` brace stays
/// `SynExpr.ForEach` — the for-loop's `in` balances `CtxtFor`, not `JOIN_IN`.
#[test]
fn diff_ast_ce_for_in_inside_query_is_not_join() {
    assert_asts_match("query { for x in xs do () }\n");
}

/// An `(` adjacent to the join `in` (`query { a in(b) }`) — the RHS is a plain
/// `Paren b`, **not** a high-precedence application. `JOIN_IN` is not an
/// atomic-expr-end, so no `HIGH_PRECEDENCE_PAREN_APP` is inserted before the
/// `(`; pins that the `in`→`JoinIn` rewrite leaves the following adjacency
/// classification matching FCS.
#[test]
fn diff_ast_ce_join_in_adjacent_paren() {
    assert_asts_match("query { a in(b) }\n");
}

/// A join clause as the **body of a `for … do`** inside the query CE
/// (`query { for x in xs do join y in ys on (x = y) }`). FCS builds
/// `ForEach(pat = x, enumExpr = xs, body = JoinIn(App(join, y), …))`: the
/// for-loop's own `in` balances `CtxtFor`, while the join's `in` — whose head
/// is `Vanilla` over the brace, with the intervening `for`/`do` contexts
/// skipped by `detect_join_in_ctxt` — is rewritten to `JOIN_IN`. Pins that the
/// join detection survives an enclosing `for … do`: the `in` must *balance*
/// the head so it is not force-closed down to the `CtxtFor` and emitted raw.
#[test]
fn diff_ast_ce_join_in_for_do_body() {
    assert_asts_match("query { for x in xs do join y in ys on (x = y) }\n");
}

/// The same `for … do` / join shape laid out over multiple lines — `do` ends
/// its line and the `join` clause is the offside do-body on the next. Pins
/// that the join `in`'s balance survives the do-body's own offside block (the
/// `Vanilla` head sits over the do-body seq-block, which `detect_join_in_ctxt`
/// skips to reach the brace).
#[test]
fn diff_ast_ce_join_in_for_do_body_multiline() {
    assert_asts_match("query {\n    for x in xs do\n    join y in ys on (x = y)\n}\n");
}

/// The join LHS ends in a **parenthesised** sub-expression whose `)` is
/// LexFilter-swallowed (`query { f(a) in xs }`). FCS builds
/// `JoinIn(App(f, Paren a), xs)`. The `Virtual::JoinIn` surfaces as the
/// immediate filtered successor of the paren body (the `)` is stripped), so the
/// join continuation must apply the same raw swallowed-closer gate as cons:
/// decline *inside* the paren body, let the `)` be recovered, then take the
/// join against the whole `App(f, Paren a)` — not mis-nest it as
/// `f(Paren(JoinIn(a, xs)))`.
#[test]
fn diff_ast_ce_join_in_lhs_ends_in_paren() {
    assert_asts_match("query { f(a) in xs }\n");
}

/// The join `in` sits on its **own line**, at/left of the `Vanilla` anchor of
/// the preceding clause (`query {\n  join x\n  in xs\n}`). FCS rewrites the `in`
/// to `JOIN_IN` *before* the offside Vanilla/SeqBlock pops run, so it is a
/// continuation (`JoinIn(App(join, x), xs)`), not a new statement separated by
/// an `OBLOCKSEP`. Pins that the rewrite beats the offside-pop ordering.
#[test]
fn diff_ast_ce_join_in_on_own_line() {
    assert_asts_match("query {\n  join x\n  in xs\n}\n");
}

/// The RHS is a leading **open-lower range** `..b` (`query { a in ..b }`), one
/// of the `declExpr` forms `parse_pratt_expr` can't consume (a leading `..`).
/// FCS builds `JoinIn(a, IndexRange(None, Some b))`; the RHS path must delegate
/// to the open-lower-range production rather than panic.
#[test]
fn diff_ast_ce_join_in_open_lower_range_rhs() {
    assert_asts_match("query { a in ..b }\n");
}

/// The RHS is a **bounded** range `b..c` (`query { a in b..c }`). `..` binds
/// looser than `JOIN_IN`, so FCS builds `IndexRange(JoinIn(a, b), c)`: the join
/// takes only `b`, and the enclosing range level wraps the whole join. Pins the
/// precedence so the range isn't mis-folded into the RHS.
#[test]
fn diff_ast_ce_join_in_bounded_range_outside() {
    assert_asts_match("query { a in b..c }\n");
}
