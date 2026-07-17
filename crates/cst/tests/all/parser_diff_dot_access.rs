//! Differential test (`parser::parse` vs FCS): phase-10.16a postfix
//! dot-access read forms — `SynExpr.DotIndexedGet` (`expr.[index]`) and
//! `SynExpr.DotGet` (postfix `expr.Member`). The remaining deferred forms (the
//! `.[..]` slice shorthand) get clean-error (no-panic, lossless) coverage
//! instead; the non-dotted `arr[i]` indexer is phase 10.16c
//! (`parser_diff_brack_index.rs`).

use crate::common;

use crate::common::{assert_asts_match, assert_asts_match_fcs_rejects_ours_accepts};
use borzoi_cst::parser::parse;

// ---- DotIndexedGet (`expr.[index]`) reads -------------------------------

/// Phase 10.16a — `arr.[0]`: the smallest dotted indexer. FCS:
/// `DotIndexedGet(Ident "arr", Const(Int32 0), dotRange, range)`. The head
/// `arr` stays `SynExpr.Ident` (not a one-segment `LongIdent`) because the
/// `.` is followed by `[`, so `parse_ident_expr` does not extend the path.
/// This is the form real F# code hit as "trailing dot in long identifier
/// path" before this slice.
#[test]
fn diff_ast_dot_indexed_get_simple() {
    assert_asts_match("let x = arr.[0]\n");
}

/// Phase 10.16a — `a.b.[0]`: the object is a multi-segment `LongIdent`.
/// `parse_ident_expr` consumes `a.b` (the first `.` is followed by an
/// ident), then stops at the `.[`. FCS:
/// `DotIndexedGet(LongIdent ["a"; "b"], Const(Int32 0), …)`.
#[test]
fn diff_ast_dot_indexed_get_long_ident_object() {
    assert_asts_match("let x = a.b.[0]\n");
}

/// Phase 10.16a — `arr.[i, j]`: a multi-argument (tuple) index. FCS wraps
/// the index args in `SynExpr.Tuple`, which our `parse_expr` produces
/// naturally: `DotIndexedGet(Ident "arr", Tuple [Ident "i"; Ident "j"], …)`.
#[test]
fn diff_ast_dot_indexed_get_tuple_index() {
    assert_asts_match("let x = arr.[i, j]\n");
}

/// Phase 10.16a — `arr.[0].[1]`: chained indexers. The postfix tail loops,
/// so the second `.[1]` wraps the first: `DotIndexedGet(DotIndexedGet(Ident
/// "arr", Const 0), Const 1)`.
#[test]
fn diff_ast_dot_indexed_get_chained() {
    assert_asts_match("let x = arr.[0].[1]\n");
}

/// Phase 10.16a — `f arr.[0]`: an indexer in application-argument position.
/// `parse_arg_expr` → `parse_atomic_expr` gives the arg the postfix tail, so
/// FCS's `App(Ident "f", DotIndexedGet(Ident "arr", Const 0))` falls out.
#[test]
fn diff_ast_dot_indexed_get_as_app_arg() {
    assert_asts_match("let x = f arr.[0]\n");
}

/// Phase 10.16a — `f x.Bar`: a *whitespace*-separated application argument
/// keeps its own postfix `.Bar` (the dot binds to the arg, not the call):
/// `App(Ident "f", DotGet(Ident "x", ["Bar"]))`. Wait — `x.Bar` is a pure
/// ident chain, so FCS optimises it to `LongIdent ["x"; "Bar"]`, giving
/// `App(Ident "f", LongIdent ["x"; "Bar"])`. Guards that suppressing the tail
/// on *HPA* args (next test) did not also suppress it on whitespace args.
#[test]
fn diff_ast_app_arg_keeps_its_long_ident() {
    assert_asts_match("let x = f x.Bar\n");
}

// ---- DotGet (postfix `expr.Member`) reads -------------------------------

/// Phase 10.16a — `(f y).Bar`: postfix member access on a non-ident atom.
/// FCS: `DotGet(Paren(App(Ident "f", Ident "y")), dotRange, SynLongIdent
/// ["Bar"], range)`. A pure ident chain would stay `LongIdent`; `DotGet`
/// only appears because the LHS is a paren.
#[test]
fn diff_ast_dot_get_on_paren() {
    assert_asts_match("let x = (f y).Bar\n");
}

/// Phase 10.16a — `(f y).Bar.Baz`: consecutive `.member`s fold into a single
/// `SynLongIdent`, matching `mkSynDot`'s `DotGet` append arm:
/// `DotGet(Paren …, SynLongIdent ["Bar"; "Baz"])`.
#[test]
fn diff_ast_dot_get_chained_members() {
    assert_asts_match("let x = (f y).Bar.Baz\n");
}

/// Phase 10.16a — `arr.[i].Length`: `DotGet` over a `DotIndexedGet` head —
/// the indexer result is a non-ident atom, so `.Length` becomes a `DotGet`:
/// `DotGet(DotIndexedGet(Ident "arr", Ident "i"), SynLongIdent ["Length"])`.
#[test]
fn diff_ast_dot_get_on_indexer() {
    assert_asts_match("let x = arr.[i].Length\n");
}

/// Phase 10.16a — `((f y).Bar).Baz`: a `DotGet` whose LHS is a *parenthesised*
/// `DotGet`. The inner member chain must stop at the swallowed `)` rather than
/// greedily folding the outer `.Baz` into the inner path. FCS:
/// `DotGet(Paren(DotGet(Paren(App(f, y)), ["Bar"])), ["Baz"])`. Regression
/// guard for the `parse_dot_get_tail` raw-adjacency check.
#[test]
fn diff_ast_dot_get_on_paren_dot_get() {
    assert_asts_match("let x = ((f y).Bar).Baz\n");
}

/// Phase 10.16a — `(a.b).c`: a parenthesised multi-segment `LongIdent`
/// followed by a member. The inner `a.b` long-ident must stop at the swallowed
/// `)` (not absorb `.c`), so the result is `DotGet(Paren(LongIdent ["a"; "b"]),
/// ["c"])`. Regression guard for `parse_ident_expr`'s loop raw-adjacency check.
#[test]
fn diff_ast_dot_get_on_paren_long_ident() {
    assert_asts_match("let x = (a.b).c\n");
}

// ---- Deferred forms: clean error, no panic, lossless --------------------

/// A deferred form must (a) not panic, (b) round-trip losslessly (the green
/// tree's text equals the source), and (c) surface at least one parse error
/// (we don't yet accept it). FCS *does* accept these, so `assert_asts_match`
/// is not applicable; this pins the "rejects cleanly" guarantee instead.
fn assert_clean_error(source: &str) {
    let parsed = parse(source);
    assert_eq!(
        parsed.root.text().to_string(),
        source,
        "lossless round-trip violated for {source:?}",
    );
    assert!(
        !parsed.errors.is_empty(),
        "expected a parse error for deferred form {source:?}, got none",
    );
}

// Slice index ranges (`arr.[1..3]` / `arr.[2..]` / `arr.[..3]`) and the
// whole-dimension wildcard `arr.[*]` (phase 10.22a) now parse to
// `DotIndexedGet(_, IndexRange …)` — see `parser_diff_ranges.rs`. The one
// remaining deferred shorthand stays a clean error below.

/// Deferred — the slice *shorthand* `arr.[..]`. Unlike `arr.[*]` (a bare
/// `Op("*")`, now phase 10.22a) and `arr.[1..3]`, the `.[..]` here is a single
/// fused `FunkyOpName(".[..]")` token, so it exercises a distinct lexer path
/// that the range expression can't reach. A clean (non-panicking, lossless)
/// failure.
#[test]
fn diff_ast_slice_shorthand_is_clean_error() {
    assert_clean_error("let x = arr.[..]\n");
}

/// Phase 10.16a × `<-` — `arr.[0] <- 1` (`DotIndexedSet`). With `<-`
/// assignment now in the parser, a `DotIndexedGet` LHS projects through
/// `mkSynAssign` to `DotIndexedSet(Ident "arr", Const 0, Const 1)`.
#[test]
fn diff_ast_dot_indexed_set() {
    assert_asts_match("let x = arr.[0] <- 1\n");
}

/// Phase 10.16a × `<-` — `(f y).Bar <- 1` (`DotSet`). A `DotGet` LHS (non-ident
/// head) projects to `DotSet(Paren(App(f, y)), ["Bar"], Const 1)`.
#[test]
fn diff_ast_dot_set() {
    assert_asts_match("let x = (f y).Bar <- 1\n");
}

/// Phase 10.16a × `<-` — `a.b <- 1` stays `LongIdentSet`, *not* `DotSet`: a
/// pure ident chain is a `SynExpr.LongIdent`, so `mkSynAssign` takes the
/// `LongOrSingleIdent` arm. Guards the `DotGet`-vs-`LongIdent` split under `<-`.
#[test]
fn diff_ast_long_ident_set_not_dot_set() {
    assert_asts_match("let x = a.b <- 1\n");
}

/// Phase 10.16a × `<-` — `(obj).P(i) <- v` (`DotNamedIndexedPropertySet`). A
/// `DotGet` on a non-ident receiver, then a high-precedence paren-app, then
/// `<-`: `App(DotGet(Paren obj, ["P"]), i)` projects to
/// `DotNamedIndexedPropertySet(Paren obj, ["P"], i, v)`. The ident-receiver
/// form `obj.P(i) <- v` is `NamedIndexedPropertySet` (a `LongIdent` function).
#[test]
fn diff_ast_dot_named_indexed_property_set() {
    assert_asts_match("let x = (obj).P(i) <- v\n");
}

/// Phase 10.16a × `<-` — `arr.[0](i) <- v` stays `Set`: `mkSynAssign` has no
/// arm for an `App` whose function is a `DotIndexedGet`, so it takes the
/// fallback. Guards that our `_ => Set` matches FCS here (no over-eager
/// indexed-property projection).
#[test]
fn diff_ast_app_of_indexer_set_is_plain_set() {
    assert_asts_match("let x = arr.[0](i) <- v\n");
}

// The non-dotted indexer `arr[0]` (FCS's `HIGH_PRECEDENCE_BRACK_APP` →
// `App(Atomic, head, ArrayOrListComputed[…])`) now parses — phase 10.16c. Its
// full differential surface lives in `parser_diff_brack_index.rs`.

/// Phase 10.16b — `f(x).Bar` (postfix dot after a high-precedence paren-app):
/// `DotGet(App(Atomic, f, Paren x), ["Bar"])`. The dot binds to the *whole*
/// application, not the argument `(x)`. Was deferred (clean error) in 10.16a;
/// HPA×dot interleaving in the atomic tail now parses it correctly.
#[test]
fn diff_ast_dot_get_after_paren_app() {
    assert_asts_match("let x = f(x).Bar\n");
}

/// Phase 10.16b — `obj.M(x).N` (method-call chain): `obj.M` long-ident, `(x)`
/// paren-app, then `.N` → `DotGet(App(Atomic, obj.M, Paren x), ["N"])`. Same
/// HPA×dot interleaving as above.
#[test]
fn diff_ast_method_call_chain() {
    assert_asts_match("let x = obj.M(x).N\n");
}

/// Phase 10.16b — folded member chain after a paren-app: `f(x).Bar.Baz` →
/// `DotGet(App(Atomic, f, Paren x), ["Bar"; "Baz"])` (consecutive members fold
/// into one `SynLongIdent`, as in `mkSynDot`).
#[test]
fn diff_ast_dot_get_chain_after_paren_app() {
    assert_asts_match("let x = f(x).Bar.Baz\n");
}

/// Phase 10.16b — dotted index after a paren-app: `f(x).[0]` →
/// `DotIndexedGet(App(Atomic, f, Paren x), Const 0)`. The index access binds to
/// the whole application.
#[test]
fn diff_ast_dot_indexed_after_paren_app() {
    assert_asts_match("let x = f(x).[0]\n");
}

/// Phase 10.16b — application after a member access after an application:
/// `f(x).Bar(y)` → `App(Atomic, DotGet(App(Atomic, f, Paren x), ["Bar"]),
/// Paren y)`. Exercises HPA → dot → HPA interleaving in one chain.
#[test]
fn diff_ast_paren_app_dot_get_paren_app() {
    assert_asts_match("let x = f(x).Bar(y)\n");
}

/// Phase 10.16b — chained paren-apps then a member: `f(x)(y).Bar`. FCS marks
/// only the *ident*-adjacent `(x)` as high-precedence; the `(y)` after `)` is a
/// `NonAtomic` application, and the `.Bar` binds to `(y)`, giving
/// `App(NonAtomic, App(Atomic, f, Paren x), DotGet(Paren y, ["Bar"]))` —
/// i.e. `f(x) ((y).Bar)`, not `(f(x)(y)).Bar`. Our markerless second paren
/// falls to the whitespace-app loop, matching this exactly.
#[test]
fn diff_ast_curried_paren_app_dot_get() {
    assert_asts_match("let x = f(x)(y).Bar\n");
}

/// Phase 10.16b — a high-precedence application as a *whitespace* argument:
/// `g f(x)` → `App(g, App(Atomic, f, Paren x))` = `g (f(x))`. The adjacent
/// `f(x)` binds tighter than the surrounding whitespace application.
#[test]
fn diff_ast_paren_app_as_whitespace_arg() {
    assert_asts_match_fcs_rejects_ours_accepts("let y = g f(x)\n");
}

// ---- Library-only cons-cell field access (`expr.( :: ).N`) --------------
//
// FSharp.Core's mutation-based list building (`cons.( :: ).1 <- tail`) uses a
// special dot qualification — FCS's `LPAREN COLON_COLON rparen DOT INT32`
// (`pars.fsy:5351`) → `SynExpr.LibraryOnlyUnionCaseFieldGet(expr, op_ColonColon,
// fieldNum, _)`; with `<-`, `mkSynAssign` collapses it to
// `LibraryOnlyUnionCaseFieldSet(expr, op_ColonColon, fieldNum, rhs, _)`. FCS
// flags the construct as library-only (a parse *error* outside fslib), but still
// builds the node, so these use the allow-errors differential (our parser reads
// it without erroring, to serve real fslib source — like the `(or)` operator).

/// The set form — `cons.( :: ).1 <- tail`, the only shape FSharp.Core uses
/// (`local.fs`, `seqcore.fs`, `prim-types.fs`). FCS:
/// `LibraryOnlyUnionCaseFieldSet(Ident "cons", ["op_ColonColon"], 1, Ident "tail", _)`.
#[test]
fn diff_cons_field_set() {
    common::assert_asts_match_with_diagnostic("let f cons tail = cons.( :: ).1 <- tail\n", 42);
}

/// The get form (the building block; no bare get appears in FSharp.Core) —
/// `cons.( :: ).0`. FCS: `LibraryOnlyUnionCaseFieldGet(Ident "cons", ["op_ColonColon"], 0, _)`.
#[test]
fn diff_cons_field_get() {
    common::assert_asts_match_with_diagnostic("let f cons = cons.( :: ).0\n", 42);
}

/// The object can be any expression, not just an ident — `(g x).( :: ).1`
/// applies the qualification to a parenthesised application.
#[test]
fn diff_cons_field_get_on_paren_app() {
    common::assert_asts_match_with_diagnostic("let f g x = (g x).( :: ).1\n", 42);
}

/// A radix (hex) field number — FCS's grammar token here is `INT32`, which
/// admits `0x`/`0o`/`0b` spellings. `cons.( :: ).0x1` is `fieldNum = 1`.
#[test]
fn diff_cons_field_get_hex_field() {
    common::assert_asts_match_with_diagnostic("let f cons = cons.( :: ).0x1\n", 42);
}

/// The `base` receiver — `base.( :: ).1`. FCS's `BASE DOT atomicExprQualification`
/// keeps `base` a single `SynExpr.Ident` (not a one-segment `LongIdent`).
#[test]
fn diff_cons_field_get_on_base() {
    common::assert_asts_match_with_diagnostic(
        "type T() =\n    inherit System.Object()\n    member _.M c = base.( :: ).1\n",
        42,
    );
}

/// A member-chain receiver — `(g x).M.( :: ).1`. The cons-field qualification
/// applies to the whole `(g x).M` DotGet (the outer postfix loop catches it
/// after the `.M` segment stops at `::`).
#[test]
fn diff_cons_field_get_on_member_chain() {
    common::assert_asts_match_with_diagnostic("let h g x = (g x).M.( :: ).1\n", 42);
}

/// An int32-suffixed field number — `cons.( :: ).1l` (lowercase `l` is the int32
/// suffix; FCS lexes it `INT32`). `0x1l` combines the suffix with a radix body.
#[test]
fn diff_cons_field_get_int32_suffix() {
    common::assert_asts_match_with_diagnostic("let f cons = cons.( :: ).1l\n", 42);
    common::assert_asts_match_with_diagnostic("let f cons = cons.( :: ).0x1l\n", 42);
}

/// A high-bit field number is decoded as signed `int` (two's-complement),
/// exactly as FCS: `cons.( :: ).0xFFFFFFFF` is `fieldNum = -1`, and the
/// u32-overflowing decimal `2147483648` is `-2147483648`.
#[test]
fn diff_cons_field_get_high_bit_field() {
    common::assert_asts_match_with_diagnostic("let f cons = cons.( :: ).0xFFFFFFFF\n", 42);
    common::assert_asts_match_with_diagnostic("let f cons = cons.( :: ).2147483648\n", 42);
}
