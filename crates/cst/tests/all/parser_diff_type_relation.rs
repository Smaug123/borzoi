//! Differential test (`parser::parse` vs FCS): the three type-relation
//! expression operators —
//!
//! * `e :? T`  → `SynExpr.TypeTest(expr, targetType, range)`
//!   (`declExpr COLON_QMARK typ`, `pars.fsy:4634`)
//! * `e :> T`  → `SynExpr.Upcast(expr, targetType, range)`
//!   (`declExpr COLON_GREATER typ`, `pars.fsy:4642`)
//! * `e :?> T` → `SynExpr.Downcast(expr, targetType, range)`
//!   (`declExpr COLON_QMARK_GREATER typ`, `pars.fsy:4650`)
//!
//! All three take a full `declExpr` on the left and a *type* (`typ`,
//! arrow/tuple/generic-inclusive) on the right, and produce a distinct AST
//! node rather than the two-tier `mkSynInfix` `App` shape. Precedence
//! (`pars.fsy:358`/`:363`, both `%left`): `:>` / `:?>` sit just below the
//! compare bucket; `:?` sits between `::` and `+`/`-`.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::SyntaxKind;

// ---- the bare forms ------------------------------------------------------

/// `a :?> b` — the motivating case (downcast).
#[test]
fn diff_ast_downcast_bare() {
    assert_asts_match("let foo = a :?> b\n");
}

/// `a :> b` — upcast.
#[test]
fn diff_ast_upcast_bare() {
    assert_asts_match("let foo = a :> b\n");
}

/// `a :? b` — type test.
#[test]
fn diff_ast_typetest_bare() {
    assert_asts_match("let foo = a :? b\n");
}

// ---- richer right-hand-side types ---------------------------------------

/// The RHS is a full `typ`, so a generic application stays inside the cast.
#[test]
fn diff_ast_downcast_generic_type() {
    assert_asts_match("let foo = a :?> List<int>\n");
}

/// A dotted type path on the RHS.
#[test]
fn diff_ast_upcast_dotted_type() {
    assert_asts_match("let foo = a :> System.Object\n");
}

/// A function-arrow RHS type — `typ` includes `->`, so the whole arrow type
/// is the cast target.
#[test]
fn diff_ast_downcast_arrow_type() {
    assert_asts_match("let foo = a :?> (int -> string)\n");
}

/// A type-test against a generic type.
#[test]
fn diff_ast_typetest_generic_type() {
    assert_asts_match("let foo = a :? option<int>\n");
}

// ---- left-hand-side richness --------------------------------------------

/// The LHS is a full `declExpr`: a parenthesised expression upcasts fine.
#[test]
fn diff_ast_upcast_paren_lhs() {
    assert_asts_match("let foo = (a + b) :> obj\n");
}

/// A member-access LHS.
#[test]
fn diff_ast_downcast_dot_get_lhs() {
    assert_asts_match("let foo = x.Value :?> string\n");
}

// ---- precedence interaction with expression operators -------------------

/// `a + b :?> c` — `:?>` (lbp below `+`) does not absorb the `+`, so this is
/// `(a + b) :?> c`.
#[test]
fn diff_ast_downcast_below_plus() {
    assert_asts_match("let foo = a + b :?> c\n");
}

/// `a :?> b + c` — the cast's RHS is a *type*, so `b` is the type and `+ c`
/// applies to the whole `Downcast`: `(a :?> b) + c`.
#[test]
fn diff_ast_downcast_then_plus() {
    assert_asts_match("let foo = a :?> b + c\n");
}

/// `a < b :? T` — `:?` binds tighter than the compare bucket, so this is
/// `a < (b :? T)`.
#[test]
fn diff_ast_typetest_above_compare() {
    assert_asts_match("let foo = a < b :? T\n");
}

/// `a < b :> T` — `:>` binds *looser* than the compare bucket, so this is
/// `(a < b) :> T`.
#[test]
fn diff_ast_upcast_below_compare() {
    assert_asts_match("let foo = a < b :> T\n");
}

// ---- associativity / chaining -------------------------------------------

/// `a :?> b :?> c` — `%left`, so `(a :?> b) :?> c`.
#[test]
fn diff_ast_downcast_left_assoc_chain() {
    assert_asts_match("let foo = a :?> b :?> c\n");
}

/// Mixed cast chain `a :> b :?> c` — both at the same precedence band,
/// left-associative: `(a :> b) :?> c`.
#[test]
fn diff_ast_upcast_downcast_chain() {
    assert_asts_match("let foo = a :> b :?> c\n");
}

// ---- composition with surrounding constructs ----------------------------

/// A cast as a function argument must be parenthesised (it's a `declExpr`).
#[test]
fn diff_ast_downcast_as_paren_arg() {
    assert_asts_match("let foo = f (a :?> b)\n");
}

/// A cast as a tuple element.
#[test]
fn diff_ast_upcast_tuple_element() {
    assert_asts_match("let foo = (a :> obj, b :> obj)\n");
}

/// A cast as a top-level `do` statement piped into `ignore`.
#[test]
fn diff_ast_typetest_top_level() {
    assert_asts_match("x :? string |> ignore\n");
}

// ---- divergence guards (parser-level, not against the diff harness) ------

/// `a :> T <- v` — FCS's `<-` LHS is a `minusExpr` (`pars.fsy:4661`), but a
/// cast is a `declExpr`, so FCS reports `<-` as unexpected ("Unexpected symbol
/// '<-' in binding") rather than building an assignment to the cast. Our parser
/// must likewise *not* silently accept it: the `<-` is left for enclosing
/// recovery, which records an error. We still emit the `UPCAST_EXPR` (built
/// before the `<-`) and stay lossless — never a no-error `ASSIGN_EXPR` wrapping
/// the cast. (FCS's `FromParseError` recovery wrapper isn't modelled by the diff
/// harness, so this is a parser-level assertion.)
#[test]
fn assignment_after_cast_is_rejected_not_silently_wrapped() {
    let src = "let foo = a :> b <- c\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "`{src}` must record an error (FCS rejects `<-` after a cast), not silently accept it",
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::UPCAST_EXPR),
        "the cast built before the `<-` must survive as an UPCAST_EXPR; tree:\n{parse:#?}",
    );
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::ASSIGN_EXPR),
        "must not wrap the cast in an ASSIGN_EXPR (FCS treats `<-` here as unexpected)",
    );
    assert_eq!(
        parse.root.text().to_string(),
        src,
        "round-trip must stay lossless on the recovery path",
    );
}

/// `a < b :? T <- c` — the looser-infix variant of the same rule: the inner
/// frame correctly declines to assign to `b :? T`, leaving the `<-` to surface
/// at the outer frame, where the built node is the comparison `a < (b :? T)` —
/// also not a `minusExpr`. FCS rejects the `<-` here too ("Unexpected symbol
/// '<-' in binding"), so we must not wrap the comparison in an `ASSIGN_EXPR`;
/// the `<-` is left for enclosing recovery (error), staying lossless.
#[test]
fn assignment_after_infix_wrapped_cast_is_rejected() {
    let src = "let foo = a < b :? T <- c\n";
    let parse = parse(src);
    assert!(
        !parse.errors.is_empty(),
        "`{src}` must record an error (FCS rejects the `<-`), not silently accept it",
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::TYPE_TEST_EXPR),
        "the type-test built before the `<-` must survive; tree:\n{parse:#?}",
    );
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::ASSIGN_EXPR),
        "must not wrap the comparison in an ASSIGN_EXPR",
    );
    assert_eq!(parse.root.text().to_string(), src, "lossless on recovery");
}

// ---- spaced / deprecated generic cast targets ---------------------------
//
// A cast target written with **spaced** generic type arguments —
// `a :> Foo < Bar >` — is FCS's `appTypeCon typeArgsNoHpaDeprecated`
// (`pars.fsy:6596`), the bare `typeArgsActual` arm: accepted as a
// `SynType.App` with warning FS1190 (`parsNonAdjacentTyargs`). Since our
// parser has no warning channel, we build the same `APP_TYPE` as the
// adjacent `Foo<Bar>` form and emit no error, so these diff cleanly
// against FCS (whose AST dump ignores the warning).

/// Spaced upcast target — `a :> Foo < Bar >`.
#[test]
fn diff_ast_upcast_spaced_generic_target() {
    assert_asts_match("let foo = a :> Foo < Bar >\n");
}

/// Spaced, dotted downcast target — `a :?> Foo.Bar < Baz >`. The dotted
/// path is absorbed greedily into one `appTypeCon` head, so this routes
/// through the same prefix-app site as the non-dotted form.
#[test]
fn diff_ast_downcast_spaced_dotted_generic_target() {
    assert_asts_match("let foo = a :?> Foo.Bar < Baz >\n");
}

/// Spaced type-test target — `a :? List < int >`.
#[test]
fn diff_ast_typetest_spaced_generic_target() {
    assert_asts_match("let foo = a :? List < int >\n");
}

/// Nested spaced generics — `a :> Foo < Bar < Baz > >`.
#[test]
fn diff_ast_upcast_spaced_nested_generic_target() {
    assert_asts_match("let foo = a :> Foo < Bar < Baz > >\n");
}

/// Multi-arg spaced generics — `a :> Map < int , string >`.
#[test]
fn diff_ast_upcast_spaced_multi_arg_generic_target() {
    assert_asts_match("let foo = a :> Map < int , string >\n");
}

/// Empty spaced type-arg list — `a :> Foo < >` (FCS's `LESS GREATER`
/// arm). The space is load-bearing: adjacent `<>` lexes as the inequality
/// operator.
#[test]
fn diff_ast_upcast_spaced_empty_generic_target() {
    assert_asts_match("let foo = a :> Foo < >\n");
}

/// Regression guard for the raw-stream `<` gate: a typed paren followed by
/// a comparison — `(x : int) < y` — must stay a comparison, not have its
/// `< y` swallowed as a spaced type-arg list on the annotation type.
/// LexFilter swallows the typed paren's `)`, so the filtered cursor shows
/// `<` after `int` while the raw cursor is at the `)`; the gate declines.
#[test]
fn diff_ast_typed_paren_then_comparison_not_a_spaced_tyarg() {
    assert_asts_match("let foo = (x : int) < y\n");
}

// ---- unterminated type-arg lists on a cast target error ------------------
//
// In *type* position (after a cast operator's `appTypeCon` target) FCS
// shifts a `<` — adjacent-without-marker or spaced — as a type-arg opener,
// then errors when no closing `>` follows (`pars.fsy:6611` reaching the
// `typeArgsActual` recover arm). So `a :?> b < c` is *not* the comparison
// `(a :?> b) < c` — that requires explicit parens — but an unterminated
// type application on `b`. We mirror FCS by erroring; a full AST diff is
// skipped because FCS's recovery tree has nodes our normaliser does not
// model. The closed forms (`a :?> b < c >`) are the diff-tested happy
// path above.

/// Spaced unterminated cast target — `a :?> b < c` (no closing `>`). FCS
/// errors; so must we (the leftover stays lossless, no comparison is
/// silently produced).
#[test]
fn spaced_unterminated_cast_target_errors() {
    let p = parse("let foo = a :?> b < c\n");
    assert!(
        !p.errors.is_empty(),
        "unterminated spaced type-arg list on a cast target must error (FCS does too)",
    );
    assert_eq!(p.root.text().to_string(), "let foo = a :?> b < c\n");
}

/// Adjacent unterminated cast target — `a :?> b<c` (no closing `>`). The
/// LexFilter withholds the typar-bracket marker, but in type position the
/// bare `<` is still a type-arg opener, so FCS errors — and so must we.
#[test]
fn adjacent_unterminated_cast_target_errors() {
    let p = parse("let foo = a :?> b<c\n");
    assert!(
        !p.errors.is_empty(),
        "unterminated adjacent type-arg list on a cast target must error (FCS does too)",
    );
    assert_eq!(p.root.text().to_string(), "let foo = a :?> b<c\n");
}

/// The parenthesised comparison `(a :?> b) < c` is the way to write a
/// less-than against a cast — the parens delimit the cast so `< c` is a
/// real comparison. FCS parses this cleanly; pin that we agree (diffed).
#[test]
fn diff_ast_parenthesised_cast_then_comparison() {
    assert_asts_match("let foo = (a :?> b) < c\n");
}
