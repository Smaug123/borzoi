//! Differential test (`parser::parse` vs FCS): phase-10.22 range / slice
//! expressions — `SynExpr.IndexRange` (`lower..upper`). Covers the general
//! `..` operator both as a slice index (`arr.[2..]`) and as a list / array /
//! `for` range (`[1..10]`, `for i in 1..10`), plus the open-bound and
//! left-associative chaining forms. The lexer fuses `int..` into one
//! `IntDotDot` token; the lex-filter split (phase 10.22, Slice A) is pinned
//! separately in `lexfilter_diff::ranges`.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;

/// A form FCS rejects (or that we defer): the parse must round-trip losslessly
/// and surface at least one error, never panic. Pins the "rejects cleanly"
/// guarantee where `assert_asts_match` is N/A.
fn assert_clean_error(source: &str) {
    let parsed = std::panic::catch_unwind(|| parse(source))
        .unwrap_or_else(|_| panic!("parser panicked on {source:?}"));
    assert_eq!(
        parsed.root.text().to_string(),
        source,
        "lossless round-trip violated for {source:?}",
    );
    assert!(
        !parsed.errors.is_empty(),
        "expected a parse error for {source:?}, got none",
    );
}

// ---- bare range expressions --------------------------------------------

/// The smallest closed range: `1..10` → `IndexRange(Some 1, Some 10)`. The
/// fused `IntDotDot("1..")` lex-filter-splits to `Int / DotDot`, so the
/// parser sees `1`, `..`, `10`.
#[test]
fn diff_ast_range_closed() {
    assert_asts_match("let r = 1..10\n");
}

/// `-1..2` — FCS's LexFilter yields a signed `Int32`, then `DotDot`. Our
/// lexer initially fuses the magnitude and range opener as `IntDotDot("1..")`;
/// the lex-filter split happens before sign-folding, so this must still become
/// `IndexRange(Some -1, Some 2)` rather than prefix-minus over a range.
#[test]
fn diff_ast_range_negative_lower_bound() {
    assert_asts_match("let r = -1..2\n");
}

/// Spaced `1 .. 10` lexes without fusion (`Int / DotDot / Int`); same tree as
/// the glued form.
#[test]
fn diff_ast_range_closed_spaced() {
    assert_asts_match("let r = 1 .. 10\n");
}

/// Open-upper `3..` → `IndexRange(Some 3, None)`: no expression-start after
/// the `..` (here the block close), so the upper bound is absent.
#[test]
fn diff_ast_range_open_upper() {
    assert_asts_match("let r = 3..\n");
}

/// Open-lower `..3` → `IndexRange(None, Some 3)`: a leading `..` (FCS's
/// `DOT_DOT declExpr` production), admitted as a `declExpr`-level starter.
#[test]
fn diff_ast_range_open_lower() {
    assert_asts_match("let r = ..3\n");
}

/// Identifier bounds: `a..b` → `IndexRange(Some(Ident a), Some(Ident b))`.
#[test]
fn diff_ast_range_idents() {
    assert_asts_match("let r = a..b\n");
}

/// `..` binds looser than arithmetic: `1+2..3*4` → `IndexRange(App(+,1,2),
/// App(*,3,4))`.
#[test]
fn diff_ast_range_binds_below_arithmetic() {
    assert_asts_match("let r = 1+2..3*4\n");
}

/// Left-associative chaining: `a..b..c` → `IndexRange(IndexRange(a,b), c)`
/// (the step-range surface FCS later lowers; at parse time it's nested
/// `IndexRange`s).
#[test]
fn diff_ast_range_chained_left_assoc() {
    assert_asts_match("let r = a..b..c\n");
}

/// A stepped range whose components are *integer literals* — `1..2..10`. Each
/// `<int>..` lexes as a single `IntDotDot` token (split into `Int` + `DotDot` by
/// LexFilter), so the step literal sits between two `IntDotDot`s in the raw
/// stream; the adjacent-malformed-numeric guard must not misread that as a
/// `<digits><ident>` literal (the `..` always separates them).
#[test]
fn diff_ast_range_stepped_int_literals() {
    assert_asts_match("let r = 1..2..10\n");
}

/// The stepped int range inside a list literal — `[1..2..10]`.
#[test]
fn diff_ast_range_stepped_int_in_list() {
    assert_asts_match("let xs = [1..2..10]\n");
}

/// A *descending* stepped int range — `10..1..1` (step and bounds all int
/// literals, the `ForEachRangeStepInt32` corpus shape).
#[test]
fn diff_ast_range_stepped_int_descending() {
    assert_asts_match("let xs = [10..1..1]\n");
}

/// A stepped int range as a `for` enumerable — `for i in 1..2..10 do …`.
#[test]
fn diff_ast_range_stepped_int_in_for() {
    assert_asts_match("let f () =\n    for i in 1..2..10 do\n        ()\n");
}

/// A stepped int range inside a sequence expression — `seq { 1..5..10 }`.
#[test]
fn diff_ast_range_stepped_int_in_seq() {
    assert_asts_match("let xs = seq { 1..5..10 }\n");
}

/// `..` binds tighter than `,`: `1..3, 4..5` → `Tuple(IndexRange(1,3),
/// IndexRange(4,5))`.
#[test]
fn diff_ast_range_tighter_than_comma() {
    assert_asts_match("let t = 1..3, 4..5\n");
}

/// A parenthesised lower bound: `(a)..b` → `IndexRange(Paren a, b)`. The
/// swallowed-`)` gate keeps the `..` outside the paren body.
#[test]
fn diff_ast_range_paren_lower() {
    assert_asts_match("let r = (a)..b\n");
}

/// A parenthesised open-lower range `(..3)` → `Paren(IndexRange(None, 3))`.
/// The paren body is a full `parse_expr`, so the leading `..` is admitted via
/// the shared `(`-after predicate.
#[test]
fn diff_ast_range_paren_open_lower() {
    assert_asts_match("let r = (..3)\n");
}

/// Open-lower range as a *parenthesised* infix operand: `1 + (..3)` →
/// `App(+, 1, Paren(IndexRange(None, 3)))`. (The unparenthesised `1 + ..3` is
/// a deferred clean error — see `range_open_lower_as_bare_operand_clean_error`.)
#[test]
fn diff_ast_range_paren_infix_operand() {
    assert_asts_match("let r = 1 + (..3)\n");
}

/// A struct-tuple element may be a range: `struct (1..3, 4)` →
/// `Tuple(isStruct=true, [IndexRange(1,3), Const 4])`.
#[test]
fn diff_ast_range_in_struct_tuple() {
    assert_asts_match("let x = struct (1..3, 4)\n");
}

/// An open-upper range inside parens, then an outer operator: `(a..) + b` →
/// `App(+, Paren(IndexRange(a, None)), b)`. The upper-bound parse must see the
/// swallowed `)` and stop, rather than taking the outer `+ b` as the bound.
#[test]
fn diff_ast_range_open_upper_in_paren() {
    assert_asts_match("let r = (a..) + b\n");
}

/// A *glued* numeric range in parens: `(1..3)` → `Paren(IndexRange(1, 3))`.
/// The `(`-after lookahead reads the raw stream, where the lower bound is still
/// the fused `IntDotDot("1..")` (the split is filtered-only) — it must admit
/// `IntDotDot` or this common form is rejected.
#[test]
fn diff_ast_range_glued_in_paren() {
    assert_asts_match("let r = (1..3)\n");
}

/// Glued numeric range as a high-precedence paren application argument:
/// `f(1..3)` → `App(f, Paren(IndexRange(1, 3)))` (the HPA `(`-lookahead shares
/// the same raw predicate).
#[test]
fn diff_ast_range_glued_hpa_paren() {
    assert_asts_match("let r = f(1..3)\n");
}

/// Open-upper glued range in parens: `(1..)` → `Paren(IndexRange(1, None))`.
#[test]
fn diff_ast_range_glued_open_upper_in_paren() {
    assert_asts_match("let r = (1..)\n");
}

// ---- whole-dimension wildcard `*` (phase 10.22a) -----------------------

/// The bare wildcard indexer `arr.[*]` → `DotIndexedGet(Ident "arr",
/// IndexRange(None, None))` (FCS's nullary `STAR` production). Both bounds
/// absent — the variant shape `INDEX_RANGE_EXPR > [STAR_TOK]`.
#[test]
fn diff_ast_wildcard_indexer() {
    assert_asts_match("let x = arr.[*]\n");
}

/// Wildcard as the first of a multi-dim index: `m.[*, 1]` →
/// `DotIndexedGet(_, Tuple([IndexRange(None,None), Const 1]))`.
#[test]
fn diff_ast_wildcard_first_multidim() {
    assert_asts_match("let x = m.[*, 1]\n");
}

/// Wildcard as the second index: `m.[1, *]` →
/// `Tuple([Const 1, IndexRange(None,None)])`.
#[test]
fn diff_ast_wildcard_second_multidim() {
    assert_asts_match("let x = m.[1, *]\n");
}

/// Wildcard mixed with a range in a multi-dim index: `m.[*, 1..2]` →
/// `Tuple([IndexRange(None,None), IndexRange(1,2)])`.
#[test]
fn diff_ast_wildcard_mixed_with_range() {
    assert_asts_match("let x = m.[*, 1..2]\n");
}

/// Chained wildcard indexers: `g.[*].[1]` →
/// `DotIndexedGet(DotIndexedGet(g, IndexRange(None,None)), Const 1)`.
#[test]
fn diff_ast_wildcard_chained() {
    assert_asts_match("let x = g.[*].[1]\n");
}

/// Wildcard as a list element: `[*]` →
/// `ArrayOrListComputed(isArray=false, IndexRange(None,None))`.
#[test]
fn diff_ast_wildcard_in_list() {
    assert_asts_match("let xs = [*]\n");
}

/// Wildcard as an array element: `[| * |]` →
/// `ArrayOrListComputed(isArray=true, IndexRange(None,None))`.
#[test]
fn diff_ast_wildcard_in_array() {
    assert_asts_match("let xs = [| * |]\n");
}

/// Wildcard as a non-first sequence element: `[1; *]` →
/// `ArrayOrListComputed(Sequential [Const 1; IndexRange(None,None)])`.
#[test]
fn diff_ast_wildcard_in_seq() {
    assert_asts_match("let xs = [1; *]\n");
}

/// Wildcard as a tuple element: `(1, *)` →
/// `Paren(Tuple([Const 1, IndexRange(None,None)]))`.
#[test]
fn diff_ast_wildcard_in_tuple() {
    assert_asts_match("let t = (1, *)\n");
}

/// Wildcard in a struct tuple: `struct (1, *)` →
/// `Tuple(isStruct=true, [Const 1, IndexRange(None,None)])`.
#[test]
fn diff_ast_wildcard_in_struct_tuple() {
    assert_asts_match("let t = struct (1, *)\n");
}

/// Paren-leading wildcard `( *, 1)` → `Paren(Tuple([IndexRange(None,None),
/// Const 1]))`. The `(`-after lookahead must admit `Op("*")` (a raw `Op("*")`
/// is never the `(*` comment opener, so it is safe).
#[test]
fn diff_ast_wildcard_paren_leading() {
    assert_asts_match("let t = ( *, 1)\n");
}

/// Wildcard as a high-precedence paren application argument: `f( * )` →
/// `App(f, Paren(IndexRange(None,None)))`.
#[test]
fn diff_ast_wildcard_hpa_paren() {
    assert_asts_match("let x = f( * )\n");
}

/// A standalone parenthesised `( * )` → `Paren(IndexRange(None,None))`.
/// **Deliberately oracle-backed:** semantically `( * )` is the multiplication
/// operator (`let mul = ( * )` makes `mul 6 7 = 42`), but FCS's *parser*
/// produces `Paren(IndexRange(None,None))` — the operator reinterpretation is a
/// post-parse concern. Since our differential oracle is the FCS parser, our
/// identical output is correct, *not* a wrong tree (this pins that a `( * )`
/// must keep matching FCS, contra the intuition that it is an operator value
/// like `( + )` — which FCS *does* parse as an operator ident).
#[test]
fn diff_ast_wildcard_paren_only() {
    assert_asts_match("let m = ( * )\n");
}

/// Wildcard as the LHS of an infix operator: `[ * + 1 ]` →
/// `App(+, IndexRange(None,None), Const 1)`. The wildcard is a high-precedence
/// atom, so the Pratt climber takes it as the `+` left operand.
#[test]
fn diff_ast_wildcard_infix_lhs() {
    assert_asts_match("let xs = [ * + 1 ]\n");
}

/// Wildcard infix LHS inside a tuple element: `(1, * + 1)` →
/// `Paren(Tuple([Const 1, App(+, IndexRange(None,None), Const 1)]))`.
#[test]
fn diff_ast_wildcard_infix_in_tuple() {
    assert_asts_match("let x = (1, * + 1)\n");
}

/// Wildcard as the LHS of infix multiplication: `[ * * 2 ]` →
/// `App(*, IndexRange(None,None), Const 2)` — the first `*` is the wildcard
/// atom, the second is the infix operator.
#[test]
fn diff_ast_wildcard_then_infix_star() {
    assert_asts_match("let xs = [ * * 2 ]\n");
}

/// Wildcard as a range *lower* bound: `[ * .. 3 ]` →
/// `IndexRange(Some(IndexRange(None,None)), Some 3)`. Because `*` is an atom,
/// it flows through the lower-bound parse and the `..` wraps it.
#[test]
fn diff_ast_wildcard_as_range_lower() {
    assert_asts_match("let xs = [ * .. 3 ]\n");
}

/// Wildcard as a range *upper* bound: `arr.[1..*]` →
/// `DotIndexedGet(_, IndexRange(Some 1, Some(IndexRange(None,None))))`.
#[test]
fn diff_ast_wildcard_as_range_upper() {
    assert_asts_match("let x = arr.[1..*]\n");
}

/// Wildcard mixed with an infix-LHS wildcard across dims: `m.[*, * + 1]` →
/// `Tuple([IndexRange(None,None), App(+, IndexRange(None,None), Const 1)])`.
#[test]
fn diff_ast_wildcard_multidim_with_infix() {
    assert_asts_match("let x = m.[*, * + 1]\n");
}

/// Wildcard as an infix *RHS*: `[1 + *]` →
/// `App(+, Const 1, IndexRange(None,None))`. The Pratt RHS lookahead
/// (`is_expr_start_at`) must admit `Op("*")` so the climber parses the atom.
#[test]
fn diff_ast_wildcard_infix_rhs() {
    assert_asts_match("let xs = [1 + *]\n");
}

/// Wildcard on both sides of infix multiplication: `[* * *]` →
/// `App(*, IndexRange(None,None), IndexRange(None,None))` — the outer `*`s are
/// wildcard atoms, the middle one the infix operator.
#[test]
fn diff_ast_wildcard_both_sides_of_infix() {
    assert_asts_match("let xs = [* * *]\n");
}

// ---- slice indexers (`arr.[…]`) ----------------------------------------

/// The reported bug — `argv.[2..]` → `DotIndexedGet(Ident "argv",
/// IndexRange(Some 2, None))`.
#[test]
fn diff_ast_slice_open_upper() {
    assert_asts_match("let defines = argv.[2..]\n");
}

/// Closed slice `arr.[1..3]` → `DotIndexedGet(_, IndexRange(Some 1, Some 3))`
/// (was the 10.16a deferred clean-error case).
#[test]
fn diff_ast_slice_closed() {
    assert_asts_match("let x = arr.[1..3]\n");
}

/// Open-lower slice `arr.[..3]` → `DotIndexedGet(_, IndexRange(None,
/// Some 3))`.
#[test]
fn diff_ast_slice_open_lower() {
    assert_asts_match("let x = arr.[..3]\n");
}

/// Multi-dimension slice `m.[1..2, 3..4]` → the index is a `Tuple` of two
/// `IndexRange`s (the `..` binds tighter than the `,`).
#[test]
fn diff_ast_slice_multidim() {
    assert_asts_match("let x = m.[1..2, 3..4]\n");
}

/// Mixed index + range `m.[i, 1..3]` → `Tuple(Ident i, IndexRange(1, 3))`.
#[test]
fn diff_ast_slice_mixed_index_and_range() {
    assert_asts_match("let x = m.[i, 1..3]\n");
}

// ---- ranges in list / array / for --------------------------------------

/// List range `[1..10]` → `ArrayOrListComputed(isArray=false,
/// IndexRange(1, 10))`.
#[test]
fn diff_ast_range_in_list() {
    assert_asts_match("let xs = [1..10]\n");
}

/// Array range `[| 1..10 |]` → `ArrayOrListComputed(isArray=true,
/// IndexRange(1, 10))`.
#[test]
fn diff_ast_range_in_array() {
    assert_asts_match("let xs = [| 1..10 |]\n");
}

/// `for i in 1..10 do …` — the enumerable is an `IndexRange`.
#[test]
fn diff_ast_range_for_loop() {
    assert_asts_match("let f () = for i in 1..10 do ()\n");
}

// ---- rejected / deferred forms (clean error, never a panic) -------------

/// FCS rejects a leading `..` as the operand of a unary prefix — `- ..3` /
/// `& ..3` ("Unexpected symbol '..'"): an open-lower range is a `declExpr`,
/// not the `minusExpr` those operands require. We must reject cleanly, not
/// recurse into the atomic const parser (which previously `unreachable!`d).
#[test]
fn diff_ast_range_as_prefix_operand_clean_error() {
    assert_clean_error("let x = - ..3\n");
    assert_clean_error("let x = & ..3\n");
}

/// FCS rejects a bare `..` (both bounds absent) — that shape is the separate
/// `*` whole-dimension production, not `..`. We report it but recover the node
/// losslessly.
#[test]
fn diff_ast_bare_dotdot_clean_error() {
    assert_clean_error("let r = ..\n");
    assert_clean_error("let xs = [ .. ]\n");
}

/// Deferred: an open-lower range as a *bare* (unparenthesised) infix operand,
/// `1 + ..3`. FCS accepts it (`App(+, 1, IndexRange(None, 3))`) via its flat
/// `declExpr OP declExpr` grammar, but our layering parses infix operands below
/// the range level, so it lands as a clean error. The parenthesised form
/// `1 + (..3)` works (see `diff_ast_range_paren_infix_operand`).
#[test]
fn diff_ast_range_open_lower_as_bare_operand_clean_error() {
    assert_clean_error("let r = 1 + ..3\n");
}

/// A chained open-lower range nests on the upper side:
/// `..a .. b` → `IndexRange(None, IndexRange(a, b))`. This is distinct from
/// the ordinary left-bounded chain `a..b..c`, which remains left-associative.
#[test]
fn diff_ast_range_chained_open_lower_upper() {
    assert_asts_match("let r = ..a .. b\n");
}

/// A nested open-lower range can also be the upper bound directly:
/// `.. ..3` → `IndexRange(None, IndexRange(None, 3))`.
#[test]
fn diff_ast_nested_open_lower_upper() {
    assert_asts_match("let r = .. ..3\n");
    assert_clean_error("let r = .. ..\n");
}

/// Regression guard for the `..`-in-operand-position panic: dropping each range
/// fragment into a wide set of prefix / infix / paren / bracket / tuple
/// contexts must never panic (lossless recovery only).
#[test]
fn range_fragments_never_panic() {
    let frags = [
        "..",
        "..3",
        "3..",
        "1..3", // single ranges
        ".. ..3",
        "1.. ..2",
        ".. ..",
        "1....2",
        "..^3",
        "3..^", // nested / fused / from-end
        "*",
        "1..*",
        "*..3",
        "* *",
        "*..",
        "..*", // wildcard interactions
        "* x",
        "* .Length",
        "* (1)",
        "*.[0]",
        "* 1 2", // wildcard application / postfix (rejected)
        "* <- 1",
        "* <-",
        "* :> obj",
        "* :? int", // wildcard as binary LHS (only `<-` rejected)
    ];
    let contexts: &[&str] = &[
        "let x = - {F}\n",
        "let x = & {F}\n",
        "let x = && {F}\n",
        "let x = % {F}\n",
        "let x = 1 + {F}\n",
        "let x = f {F}\n",
        "let x = ({F})\n",
        "let x = [{F}]\n",
        "let x = [|{F}|]\n",
        "let x = 1, {F}\n",
        "let x = struct ({F}, 2)\n",
        "let x = arr.[{F}]\n",
        "let x = m.[{F}, {F}]\n",
        "let x = !{F}\n",
        "let x = (fun y -> {F})\n",
        "let x = 1 + ({F})\n",
        "let x = {F}\n",
        // infix-RHS position (`is_expr_start_at` lookahead past the operator).
        "let x = [1 + {F}]\n",
        "let x = [{F} * {F}]\n",
        // prefix-operand sites that recurse into `parse_minus_expr` — a `..`
        // leaf is a clean error and a `*` atom flows through, but neither may
        // panic (`upcast`/`downcast` are the `parse_inferred_cast` operands).
        "let x = upcast {F}\n",
        "let x = downcast {F}\n",
    ];
    for c in contexts {
        for f in &frags {
            let src = c.replace("{F}", f);
            let r = std::panic::catch_unwind(|| {
                let p = parse(&src);
                assert_eq!(p.root.text().to_string(), src, "lossless fail {src:?}");
            });
            assert!(r.is_ok(), "parser panicked on {src:?}");
        }
    }
}

/// A glued numeric range `(1..3)` is a valid expression in every full-expression
/// position; the raw-stream `(`-lookahead must admit the fused `IntDotDot` so
/// none of these is wrongly rejected. Asserts a *clean* parse (zero errors),
/// stronger than the no-panic guard.
#[test]
fn glued_paren_ranges_parse_clean() {
    for src in [
        "let r = (1..3)\n",
        "let r = (1..)\n",
        "let r = f(1..3)\n",
        "let r = [(1..3)]\n",
        "let r = [|(1..3)|]\n",
        "let r = ((1..3))\n",
        "let r = (1..3), (4..6)\n",
        "let r = (1..3) + x\n",
        "let r = g (1..3)\n",
        "let r = h(1..3, 4..6)\n",
    ] {
        let p = parse(src);
        assert_eq!(p.root.text().to_string(), src, "lossless fail {src:?}");
        assert!(
            p.errors.is_empty(),
            "glued paren range should parse clean: {src:?} -> {:?}",
            p.errors
        );
    }
}

/// A wildcard `*` is a valid expression in every full-expression position FCS
/// accepts. Clean-parse sweep (zero errors), stronger than the no-panic guard —
/// pins that admitting `*` as a `declExpr` starter doesn't perturb its
/// neighbours.
#[test]
fn wildcard_parses_clean() {
    for src in [
        "let x = arr.[*]\n",
        "let x = m.[*, *]\n",
        "let x = m.[*, 1..2]\n",
        "let xs = [*]\n",
        "let xs = [| * |]\n",
        "let xs = [1; *]\n",
        "let t = (1, *)\n",
        "let t = struct (1, *)\n",
        "let x = g.[*].[1]\n",
        "let x = f [*]\n",
    ] {
        let p = parse(src);
        assert_eq!(p.root.text().to_string(), src, "lossless fail {src:?}");
        assert!(
            p.errors.is_empty(),
            "wildcard should parse clean: {src:?} -> {:?}",
            p.errors
        );
    }
}

/// FCS rejects a bare `*` at a top-level expression *head* — the RHS `let r =
/// *`, or the operand of a `-`/`&`/`upcast` prefix — on the offside rule
/// ("this token is offside…"): the `*` opens no context, so the RHS block's
/// anchor lands at EOF, offside of the `let`. Since the §A offside FS0058
/// emission landed we report the matching FS0058 at that same EOF span (the
/// former lenient acceptance is closed). We still build a lossless tree
/// (`IndexRange(None, None)` / `App(~-, …)` / `AddressOf(…)` / `Upcast(…)`);
/// for `let r = *` that tree also matches FCS's recovery (pinned in
/// `parser_diff_offside.rs`), while the `-`/`&`/`upcast` operand cases recover
/// to a different tree than FCS — a separate, pre-existing recovery gap — so
/// this test asserts only the shared offside diagnostic, not tree parity.
#[test]
fn bare_head_wildcard_flags_offside_at_eof() {
    for src in [
        "let r = *\n",
        "let x = - *\n",
        "let x = & *\n",
        "let x = upcast *\n",
    ] {
        let p = parse(src);
        assert_eq!(p.root.text().to_string(), src, "lossless {src:?}");
        // Exactly the offside FS0058 at EOF, matching FCS's span.
        assert_eq!(
            p.errors.len(),
            1,
            "one offside error for {src:?}: {:?}",
            p.errors
        );
        assert_eq!(
            p.errors[0].span,
            src.len()..src.len(),
            "offside span at EOF for {src:?}",
        );
        assert!(
            p.errors[0].message.contains("offside"),
            "offside message for {src:?}: {}",
            p.errors[0].message,
        );
    }
}

/// The wildcard is a `declExpr` leaf, **not** an `atomicExpr`: it cannot be
/// applied (`* x`) or carry a postfix `.member` / `.[i]` / `(args)` tail. FCS
/// rejects all of these ("Unexpected …"); our parser emits the `*` at the
/// `minusExpr` level (above the application / postfix machinery), so the
/// following token is left unconsumed and reported, never folded into a wrong
/// `App(IndexRange, …)` / `DotGet(IndexRange, …)` tree.
#[test]
fn wildcard_application_and_postfix_clean_error() {
    for src in [
        "let xs = [* x]\n",
        "let xs = [* .Length]\n",
        "let x = arr.[1..* (n)]\n",
        "let x = m.[* x, 1]\n",
    ] {
        assert_clean_error(src);
    }
}

/// The wildcard is a `declExpr` leaf, **not** a `minusExpr`, so it cannot be the
/// LHS of a `<-` assignment (FCS's `<-` binds a `minusExpr`). `* <- 1` is an FCS
/// error; the `<-` gate excludes a bare-wildcard LHS (the `built_continuation` /
/// cast story), leaving the `<-` for recovery rather than building a wrong
/// `ASSIGN_EXPR` with a wildcard target. (A wildcard *index* — `arr.[*] <- v` —
/// is unaffected: its LHS is the `DotIndexedGet`, a real `minusExpr`.)
#[test]
fn wildcard_as_assignment_lhs_clean_error() {
    assert_clean_error("let x = * <- 1\n");
}

/// The wildcard *is* a valid LHS for every other binary / `declExpr` production
/// FCS accepts: the type-relation operators (`:>` / `:?>` / `:?`) and the infix
/// operators. (`* <- 1` is the sole rejected binary form — see
/// `wildcard_as_assignment_lhs_clean_error`.)
#[test]
fn diff_ast_wildcard_as_type_relation_lhs() {
    assert_asts_match("let x = * :> obj\n");
    assert_asts_match("let x = * :? int\n");
}

/// A wildcard *index* on the LHS of `<-` is a `DotIndexedSet`: `arr.[*] <- v` →
/// `DotIndexedSet(arr, IndexRange(None,None), v)`. The wildcard here is the
/// index argument, not the assignment target, so the `<-` gate is unaffected.
#[test]
fn diff_ast_wildcard_index_assignment() {
    assert_asts_match("let x = arr.[*] <- v\n");
}

/// `arr.[*..3]` is a clean error — but a *lexer* one, not a parser deferral:
/// the glued `*..` lexes as one `Op("*..")` operator (no space), which FCS also
/// rejects ("Unexpected infix operator in expression"). The spaced `* .. 3` is
/// the real wildcard-lower-bound form and parses (see
/// `diff_ast_wildcard_as_range_lower`).
#[test]
fn glued_star_dotdot_is_clean_error() {
    assert_clean_error("let x = arr.[*..3]\n");
}

/// A leading `..` as the operand of a unary prefix is rejected cleanly (never a
/// panic) at every prefix site: `-`/`&`/`%` and the inferred casts
/// `upcast`/`downcast` (whose operand is also a `minusExpr`). FCS rejects these
/// too. Guards the third prefix-operand site (`parse_inferred_cast`) against the
/// same `unreachable!` the `-`/`&` guards prevent. (The `*` wildcard, by
/// contrast, is a high-precedence atom and flows through as the operand — FCS
/// offside-rejects the top-level `- *`, a documented lenient divergence.)
#[test]
fn open_lower_range_as_prefix_operand_clean_error() {
    for src in [
        "let x = upcast ..3\n",
        "let x = downcast ..\n",
        "let x = - ..3\n",
        "let x = & ..3\n",
    ] {
        assert_clean_error(src);
    }
}
