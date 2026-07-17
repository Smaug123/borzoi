//! Differential test (`parser::parse` vs FCS): phase-10.16c the **non-dotted**
//! high-precedence bracket indexer `arr[i]` (FCS's `HIGH_PRECEDENCE_BRACK_APP`).
//!
//! Unlike the dotted `arr.[i]` (`SynExpr.DotIndexedGet`, phase 10.16a), the
//! non-dotted form is *not* a dedicated AST node: FCS parses `arr[i]` as
//! `App(ExprAtomicFlag.Atomic, false, arr, ArrayOrListComputed [i])` — an
//! *atomic application* of the head to a bracketed list literal
//! (`pars.fsy:5242 atomicExpr HIGH_PRECEDENCE_BRACK_APP atomicExpr`). The
//! LexFilter inserts the `HighPrecedenceBrackApp` adjacency virtual **only**
//! between an *ident* and an immediately-adjacent `[` (`arr[`, no whitespace),
//! so only that case is the atomic indexer; a `[` after `)` / `]` (or with
//! whitespace) is an ordinary whitespace application of a list literal
//! (`ExprAtomicFlag.NonAtomic`), covered by the regression guards at the end.

use crate::common::assert_asts_match;

// ---- The atomic indexer (HighPrecedenceBrackApp fires) ------------------

/// Phase 10.16c — `arr[i]`: the smallest non-dotted indexer. FCS:
/// `App(Atomic, Ident "arr", ArrayOrListComputed(false, Ident "i"))`. The head
/// `arr` stays `SynExpr.Ident`; the bracket body is the *same* list-literal
/// expression as a bare `[i]`.
#[test]
fn diff_ast_brack_index_simple() {
    assert_asts_match("let z = arr[i]\n");
}

/// Phase 10.16c — `foo.bar[baz.quux]`: the originally-reported failure. The
/// head is a multi-segment `LongIdent`, the index a multi-segment `LongIdent`.
/// FCS: `App(Atomic, LongIdent ["foo"; "bar"], ArrayOrListComputed(false,
/// LongIdent ["baz"; "quux"]))`. The HPB virtual sits between `bar` and `[`,
/// so `parse_ident_expr` stops the head path at `foo.bar`.
#[test]
fn diff_ast_brack_index_long_ident_head_and_index() {
    assert_asts_match("let z = foo.bar[baz.quux]\n");
}

/// Phase 10.16c — `arr[i, j]`: a multi-argument (tuple) index. The `,` is
/// absorbed by the list element's tuple layer, so the bracket body is a single
/// `SynExpr.Tuple`: `App(Atomic, Ident "arr", ArrayOrListComputed(false,
/// Tuple [Ident "i"; Ident "j"]))`.
#[test]
fn diff_ast_brack_index_tuple() {
    assert_asts_match("let z = arr[i, j]\n");
}

/// Phase 10.16c — `arr[1..3]`: a range/slice index. The bracket body is a
/// `SynExpr.IndexRange` (phase 10.22), reached for free because the indexer
/// body is a full list-literal expression: `App(Atomic, Ident "arr",
/// ArrayOrListComputed(false, IndexRange(Const 1, Const 3)))`.
#[test]
fn diff_ast_brack_index_range() {
    assert_asts_match("let z = arr[1..3]\n");
}

/// Phase 10.16c — `arr[]`: an empty index. FCS splits the empty bracket to the
/// non-computed `SynExpr.ArrayOrList(false, [], _)` variant, so this is
/// `App(Atomic, Ident "arr", ArrayOrList(false, []))` — distinct from the
/// non-empty `ArrayOrListComputed`.
#[test]
fn diff_ast_brack_index_empty() {
    assert_asts_match("let z = arr[]\n");
}

/// Phase 10.16c — `m[a[b]]`: a nested indexer inside the index. The inner
/// `a[b]` is itself an atomic bracket app parsed inside the list body:
/// `App(Atomic, Ident "m", ArrayOrListComputed(false, App(Atomic, Ident "a",
/// ArrayOrListComputed(false, Ident "b"))))`.
#[test]
fn diff_ast_brack_index_nested() {
    assert_asts_match("let z = m[a[b]]\n");
}

/// Phase 10.16c — `obj.M[i]`: the HPB virtual fires after a *dotted* long-ident
/// head (between member `M` and `[`). FCS: `App(Atomic, LongIdent ["obj"; "M"],
/// ArrayOrListComputed(false, Ident "i"))`.
#[test]
fn diff_ast_brack_index_dotted_head() {
    assert_asts_match("let z = obj.M[i]\n");
}

// ---- Interaction with the rest of the postfix tail ----------------------

/// Phase 10.16c — `f arr[i]`: an indexer in *whitespace* application-argument
/// position. The argument `arr[i]` gets its own atomic postfix tail (it is a
/// full `parse_atomic_expr`), so FCS's `App(NonAtomic, Ident "f", App(Atomic,
/// Ident "arr", ArrayOrListComputed(false, Ident "i")))` falls out — the outer
/// application is the whitespace one, the inner the atomic indexer.
#[test]
fn diff_ast_brack_index_as_app_arg() {
    assert_asts_match("let z = f arr[i]\n");
}

/// Phase 10.16c — `arr[i][j]`: chained brackets. Only the *first* `[` is
/// ident-adjacent, so only it is atomic; the second `[` follows `]` and is a
/// markerless whitespace application: `App(NonAtomic, App(Atomic, Ident "arr",
/// [i]), [j])`.
#[test]
fn diff_ast_brack_index_chained() {
    assert_asts_match("let z = arr[i][j]\n");
}

/// Phase 10.16c — `arr[i].Length`: a postfix `.member` after the indexer. The
/// atomic tail loops, wrapping the `App` head in a `DotGet`: `DotGet(App(Atomic,
/// Ident "arr", [i]), ["Length"])`.
#[test]
fn diff_ast_brack_index_then_dot_get() {
    assert_asts_match("let z = arr[i].Length\n");
}

/// Phase 10.16c — `xs[0].Foo[1]`: index, then `.Foo` (`DotGet`), then a *second*
/// indexer whose `[` is adjacent to the member ident `Foo` (so the HPB virtual
/// fires again). The whole second application is atomic: `App(Atomic,
/// DotGet(App(Atomic, Ident "xs", [0]), ["Foo"]), [1])`. Exercises the HPB arm
/// over a non-ident (`DotGet`) head.
#[test]
fn diff_ast_brack_index_after_dot_get_member() {
    assert_asts_match("let z = xs[0].Foo[1]\n");
}

// ---- `<-` assignment LHS shapes (mkSynAssign projections) ---------------

/// Phase 10.16c × `<-` — `arr[0] <- 1`. The LHS `App(Ident "arr", [0])` has an
/// *ident* function, so `mkSynAssign` takes neither the `NamedIndexedPropertySet`
/// (LongIdent function) nor the `Dot*` arms — it falls through to the generic
/// `SynExpr.Set(App(Atomic, Ident "arr", [0]), Const 1)`.
#[test]
fn diff_ast_brack_index_set_ident_head() {
    assert_asts_match("let z = arr[0] <- 1\n");
}

/// Phase 10.16c × `<-` — `foo.bar[0] <- 1`. The LHS `App(LongIdent ["foo";
/// "bar"], [0])` has a *long-ident* function, so `mkSynAssign` projects it to
/// `NamedIndexedPropertySet(["foo"; "bar"], [0], 1)` (the same arm as
/// `foo.bar(0) <- 1` / `foo.bar 0 <- 1`).
#[test]
fn diff_ast_brack_index_set_long_ident_head() {
    assert_asts_match("let z = foo.bar[0] <- 1\n");
}

// ---- Regression: the markerless (NonAtomic) cases stay correct ----------

/// Regression — `(f y)[i]`: the `[` follows `)`, so the LexFilter emits *no*
/// HPB virtual; this is an ordinary whitespace application of the list literal,
/// `App(NonAtomic, Paren(App(f, y)), ArrayOrListComputed(false, Ident "i"))`.
/// Already parsed before 10.16c; pinned to guard the atomic-vs-non-atomic split.
#[test]
fn diff_ast_brack_index_paren_head_is_non_atomic() {
    assert_asts_match("let z = (f y)[i]\n");
}

/// Regression — `arr.[0][1]`: a dotted indexer (`DotIndexedGet`) followed by a
/// markerless bracket (`[` after `]`). The trailing `[1]` is a NonAtomic
/// whitespace application: `App(NonAtomic, DotIndexedGet(Ident "arr", Const 0),
/// ArrayOrListComputed(false, Const 1))`.
#[test]
fn diff_ast_dotted_then_non_atomic_brack() {
    assert_asts_match("let z = arr.[0][1]\n");
}

// ---- From-end index / slice (`arr.[^1]`, FCS's `SynExpr.IndexFromEnd`) -------
// A `^expr` index bound counts from the end; valid only inside an indexer. The
// `^` is `Op("^")` (split-`..^` lower bound, or a standalone/upper bound); the
// glued `..^` open-lower slice is `Token::DotDotHat`. Each from-end bound is an
// `INDEX_FROM_END_EXPR`, matching FCS's `IndexFromEnd`.

/// A single from-end index — `arr.[^1]` is `DotIndexedGet(arr, IndexFromEnd 1)`.
#[test]
fn diff_ast_from_end_single() {
    assert_asts_match("let z = arr.[^1]\n");
}

/// From-end lower bound, open upper — `arr.[^3..]` (`IndexRange(IndexFromEnd 3,
/// None)`). The `^3..` lexes `Op(^)` then a fused `IntDotDot`, split to `3` `..`.
#[test]
fn diff_ast_from_end_lower_open_upper() {
    assert_asts_match("let z = arr.[^3..]\n");
}

/// Open lower, from-end upper — `arr.[..^1]` (`IndexRange(None, IndexFromEnd 1)`).
/// The `..^` is the glued `DotDotHat` operator.
#[test]
fn diff_ast_from_end_open_lower_upper() {
    assert_asts_match("let z = arr.[..^1]\n");
}

/// Both bounds from-end — `str.[^3..^1]` (`IndexRange(IndexFromEnd 3,
/// IndexFromEnd 1)`). The upper `^1` here is a split `Op(^)` prefix (the `..`
/// glued with the `3` into `IntDotDot`), exercising the `parse_index_range_bound`
/// from-end path on the upper.
#[test]
fn diff_ast_from_end_both_bounds() {
    assert_asts_match("let z = str.[^3..^1]\n");
}

/// A from-end upper after a non-int lower (`m.[a..^1]`) surfaces the `..^` as the
/// glued `DotDotHat` (the `a` does not fuse with `..`), so the upper is from-end
/// without its own `^` prefix.
#[test]
fn diff_ast_from_end_ident_lower_hat_upper() {
    assert_asts_match("let z = m.[a..^1]\n");
}

/// Multi-dimensional slice mixing from-end and plain bounds — the corpus
/// `arr.[..^1, ^1..^0, ^2..]` shape (a `Tuple` of three `IndexRange`s).
#[test]
fn diff_ast_from_end_multidim() {
    assert_asts_match("let z = arr.[..^1, ^1..^0, ^2..]\n");
}

/// `^expr` is a `minusExpr`-level prefix in FCS, so it is valid in *general*
/// expression position, not only inside an indexer — `let i = ^1` is
/// `IndexFromEnd 1`.
#[test]
fn diff_ast_from_end_general_let_rhs() {
    assert_asts_match("let i = ^1\n");
}

/// A from-end expression as a list element — `[ ^1 ]`.
#[test]
fn diff_ast_from_end_in_list() {
    assert_asts_match("let r = [ ^1 ]\n");
}

/// A *spaced* open-lower from-end slice — `arr.[.. ^1]` (the `..` and `^` are not
/// fused into `DotDotHat`), so the `^1` is the ordinary from-end prefix on the
/// upper bound: `IndexRange(None, IndexFromEnd 1)`.
#[test]
fn diff_ast_from_end_spaced_open_lower() {
    assert_asts_match("let r = arr.[.. ^1]\n");
}

/// `^expr` as an infix RHS — `1 + ^1` is `App(+, 1, IndexFromEnd 1)` (the `+`
/// operand is a `minusExpr`, which the from-end prefix is).
#[test]
fn diff_ast_from_end_infix_rhs() {
    assert_asts_match("let z = 1 + ^1\n");
}

/// A parenthesised open-lower from-end slice — `(..^1)`. The `(`-lookahead
/// consults the raw stream (still-fused `..^`), so it must admit `DotDotHat`;
/// FCS parses it as `Paren(IndexRange(None, IndexFromEnd 1))`.
#[test]
fn diff_ast_from_end_paren_open_lower() {
    assert_asts_match("let r = (..^1)\n");
}

/// A `declExpr`-level keyword directly after the `^` prefix (`^ if …`) is invalid
/// (FCS FS0010), like `- if …`; pin that we error (not silently accept) and stay
/// lossless.
#[test]
fn from_end_keyword_operand_rejects() {
    use borzoi_cst::parser::parse;
    let src = "let x = ^ if true then 1 else 2\n";
    let p = parse(src);
    assert!(!p.errors.is_empty(), "`^ if …` must error like `- if …`");
    assert_eq!(
        p.root.text().to_string(),
        src,
        "round-trip must stay lossless"
    );
}

/// A parenthesised `^`-head-typar (`(^f)`, `(^f : int)`, `arr.[(^i)]`) is FCS's
/// SRTP trait-call form, which it reserves and errors on when incomplete — *not*
/// a from-end expression (that is `(^1)` / `arr.[^i]`, whose `^` operand is a
/// literal/ident-in-index, not a paren-head typar). Pin that we error like FCS
/// and stay lossless, rather than mis-accepting these as `IndexFromEnd`.
#[test]
fn from_end_paren_head_typar_is_trait_call_not_index() {
    use borzoi_cst::parser::parse;
    for src in [
        "let x = (^f)\n",
        "let x = (^f : int)\n",
        "let r = arr.[(^i)]\n",
    ] {
        let p = parse(src);
        assert!(
            !p.errors.is_empty(),
            "parenthesised `^ident` is SRTP-reserved and must error: {src:?}",
        );
        assert_eq!(
            p.root.text().to_string(),
            src,
            "round-trip must stay lossless: {src:?}"
        );
    }
}

/// `..^` immediately followed by another operator char (`..^+1`, `..^^1`,
/// `..^-1`) is *not* a from-end slice: FCS's maximal-munch lexer takes the whole
/// run as one operator and rejects it (FS1208). Pin that the `DotDotHat` split is
/// gated on operator-adjacency, so these error (not parse as `.. (^ …)`).
#[test]
fn from_end_dot_dot_hat_operator_adjacent_rejects() {
    use borzoi_cst::parser::parse;
    for src in [
        "let x = arr.[..^+1]\n",
        "let x = arr.[..^^1]\n",
        "let x = arr.[..^-1]\n",
    ] {
        let p = parse(src);
        assert!(!p.errors.is_empty(), "`..^`+op run must error: {src:?}");
        assert_eq!(p.root.text().to_string(), src, "lossless: {src:?}");
    }
}

/// A standalone attribute declaration followed by a from-end expression decl —
/// `[<System.Obsolete>]⏎ ^1` is `Attributes [Obsolete]` + `Expr(IndexFromEnd 1)`.
/// The module-decl attribute lookahead must recognise the raw `^` as an
/// expression start, else it falsely attaches the attribute to the expression.
#[test]
fn diff_ast_from_end_after_standalone_attribute() {
    assert_asts_match("[<System.Obsolete>]\n^1\n");
}

/// A standalone attribute declaration followed by an open-lower from-end *range*
/// decl — `[<System.Obsolete>]⏎ ..^1` is `Attributes [Obsolete]` +
/// `Expr(IndexRange(None, IndexFromEnd 1))`. The module-decl attribute lookahead
/// must recognise the raw `..^` (`DotDotHat`) / `..` range starter, not just `^`.
#[test]
fn diff_ast_from_end_range_after_standalone_attribute() {
    assert_asts_match("[<System.Obsolete>]\n..^1\n");
}

/// A parenthesised from-end expression whose `^ident` operand *continues* — `(^a.b)`,
/// `(^a + 1)`, `(^a.[0])`, and the same nested in an indexer (`arr.[(^i + 1)]`) — is an
/// ordinary `Paren(IndexFromEnd …)`, **not** the SRTP trait-call shape. The head-typar
/// recovery must fire only on the genuinely incomplete `(^a)` / `(^a : …)` forms below,
/// so a following `.`/operator/index keeps these as valid from-end expressions.
#[test]
fn diff_ast_paren_from_end_ident_continuation() {
    assert_asts_match("let x = (^a.b)\n");
    assert_asts_match("let x = (^a + 1)\n");
    assert_asts_match("let x = (^a.[0])\n");
    assert_asts_match("let r = arr.[(^i + 1)]\n");
}

/// The SRTP-reserved incomplete trait-call shapes `(^a)` and `(^a : int)` — a head
/// typar immediately closed or followed by `:` — have no ordinary from-end parse and
/// must error (matching FCS). Pins that narrowing the head-typar recovery to these
/// shapes did not also start *accepting* them.
#[test]
fn paren_incomplete_srtp_head_typar_rejects() {
    use borzoi_cst::parser::parse;
    for src in ["let x = (^a)\n", "let x = (^a : int)\n"] {
        let p = parse(src);
        assert!(!p.errors.is_empty(), "incomplete SRTP must error: {src:?}");
        assert_eq!(p.root.text().to_string(), src, "lossless: {src:?}");
    }
}
