//! Differential test (`parser::parse` vs FCS): expression-level generic type
//! application `f<int>` — FCS's `SynExpr.TypeApp(expr, lessRange, typeArgs,
//! commaRanges, greaterRange, typeArgsRange, range)` (the `atomicExpr`
//! production `atomicExpr HIGH_PRECEDENCE_TYAPP typeArgsActual`, `pars.fsy:5252`).
//!
//! The head is the preceding `atomicExpr` (an `Ident` for `f<int>`, a
//! `LongIdent` for `Seq.empty<int>`), and `typeArgs` is the `< … >` block
//! (`SynType list`). The `<` / `>` are the LexFilter's adjacency-rewritten
//! `Less(true)` / `Greater(true)`, with a `HighPrecedenceTyApp` virtual the
//! parser consumes zero-width. A `(` adjacent to the closing `>` is itself
//! marked `HighPrecedenceParenApp` by the LexFilter, so `ResizeArray<_>()`
//! parses as `App(Atomic, TypeApp(ResizeArray, [Anon]), Const Unit)` — the
//! type-application binds tighter than the (atomic) function application.
//!
//! Distinct from the *type*-level `Foo<int>` (`SynType.App`, phase 7.6), which
//! lives inside a type annotation / `new T<…>(…)` target.

use crate::common::assert_asts_match;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::SyntaxKind;

// ---- the motivating case ------------------------------------------------

/// `ResizeArray<_>()` — the report's failing snippet:
/// `App(Atomic, TypeApp(Ident "ResizeArray", [Anon]), Const Unit)`.
#[test]
fn diff_ast_type_app_resize_array_anon() {
    assert_asts_match("let tokens = ResizeArray<_>()\n");
}

// ---- bare type application (no following application) --------------------

/// `id<int>` — a bare `TypeApp(Ident "id", [int])` with no trailing args.
#[test]
fn diff_ast_type_app_bare_ident() {
    assert_asts_match("let x = id<int>\n");
}

/// `Seq.empty<int>` — a long-ident head: `TypeApp(LongIdent ["Seq"; "empty"],
/// [int])`.
#[test]
fn diff_ast_type_app_long_ident_head() {
    assert_asts_match("let x = Seq.empty<int>\n");
}

/// `typeof<int>` — the idiomatic real-world form. `typeof` is not special in
/// the syntax: still `TypeApp(Ident "typeof", [int])`.
#[test]
fn diff_ast_type_app_typeof() {
    assert_asts_match("let x = typeof<int>\n");
}

// ---- multiple / structured type arguments -------------------------------

/// Two type args — `TypeApp(Ident "d", [int; string])`, then unit application.
#[test]
fn diff_ast_type_app_two_args() {
    assert_asts_match("let x = d<int, string>()\n");
}

/// A tuple type argument — `box<int * string> v` is
/// `App(TypeApp(box, [Tuple [int; string]]), v)`, the `*` staying inside the
/// single type argument (reusing the type parser's tuple handling).
#[test]
fn diff_ast_type_app_tuple_arg() {
    assert_asts_match("let x = box<int * string> v\n");
}

/// An array type argument — `f<int[]>` is `TypeApp(f, [Array(1, int)])`, the
/// `[]` staying inside the single type argument.
#[test]
fn diff_ast_type_app_array_arg() {
    assert_asts_match("let x = f<int[]>\n");
}

/// A statically-resolved type parameter (`^T`) as the type argument, inside an
/// `inline` function: `g<^T> x` is `App(TypeApp(g, [Var ^T]), x)`. FCS parses
/// `^T` as a `Var` type with the head-typar flag.
#[test]
fn diff_ast_type_app_srtp_arg() {
    assert_asts_match("let inline f x = g<^T> x\n");
}

/// The empty type-arg list — FCS's `typeArgsActual: LESS GREATER`
/// (`pars.fsy:6649`) — yields `TypeApp(Ident "f", [])` with no error. (The
/// adjacent `<>` fuses into the `<>` inequality op, so the empty form only
/// arises spaced.)
#[test]
fn diff_ast_type_app_empty_args() {
    assert_asts_match("let x = f< >\n");
}

// ---- whitespace application of the type-applied head --------------------

/// `f<int> x` — a whitespace (`NonAtomic`) application of the type-applied
/// head: `App(NonAtomic, TypeApp(f, [int]), Ident "x")`.
#[test]
fn diff_ast_type_app_whitespace_app() {
    assert_asts_match("let a = f<int> x\n");
}

// ---- composition --------------------------------------------------------

/// As a pipeline operand — `Seq.empty<int> |> List.ofSeq` is
/// `App(App(|>, TypeApp(Seq.empty, [int])), List.ofSeq)`.
#[test]
fn diff_ast_type_app_pipeline_operand() {
    assert_asts_match("let x = Seq.empty<int> |> List.ofSeq\n");
}

/// A member access *after* the type application — `e.Foo<int>.Bar` is
/// `DotGet(TypeApp(LongIdent ["e"; "Foo"], [int]), ["Bar"])`. The postfix tail
/// loops, so the trailing `.Bar` chains onto the *whole* type application (not
/// the type argument). This exercises the type-app → `DotGet` continuation,
/// complementing the paren-app (`ResizeArray<_>()`) and whitespace-app cases.
#[test]
fn diff_ast_type_app_then_dot_get() {
    assert_asts_match("let x = e.Foo<int>.Bar\n");
}

/// The originally-reported regression: a generic instantiation immediately
/// applied to unit args, upstream of a pipeline with a lambda
/// (`e.OfType<Foo>() |> Seq.filter (fun f -> not f)`). Before expression-level
/// type application landed, the parser bailed at the `<` and drained the rest
/// of the line — including the `fun` and `)` — as raw `ERROR` tokens
/// ("unsupported token Fun rewritten by LexFilter"). All of it must now parse
/// cleanly and match FCS: it stresses the type-app → `HighPrecedenceParenApp`
/// → pipe → lambda-argument chain end to end.
#[test]
fn diff_ast_type_app_in_pipeline_with_lambda() {
    assert_asts_match("let g = e.OfType<Foo>() |> Seq.filter (fun f -> not f)\n");
}

/// A `new`-free generic construction used as a function argument must be
/// parenthesised at the call site like any atomic; here it is the whole RHS.
#[test]
fn diff_ast_type_app_dotted_generic_construct() {
    assert_asts_match("let x = System.Collections.Generic.List<int>()\n");
}

/// Nested generic type arguments — `Dictionary<int, List<string>>()` is
/// `App(Atomic, TypeApp(Dictionary, [int; App(List, [string])]), Const Unit)`.
/// The LexFilter splits the fused `>>` into two adjacency-rewritten `>` so the
/// inner and outer type-arg lists both close.
#[test]
fn diff_ast_type_app_nested_args() {
    assert_asts_match("let x = Dictionary<int, List<string>>()\n");
}

// ---- comparison vs type-application disambiguation ----------------------

/// An *unclosed* `<` is **not** a type application: FCS's LexFilter withholds
/// the `HighPrecedenceTyApp` marker when its balance walk finds no matching
/// `>`, so `f<int` stays the comparison `f < int`
/// (`App(App(op_LessThan, f), int)`) — no error, no `TypeApp`. This pins that
/// our parser agrees (the markerless `<` falls through to the infix climber).
#[test]
fn diff_ast_type_app_unclosed_is_comparison() {
    assert_asts_match("let x = f<int\n");
}

/// Type application and comparison coexisting: `f<int> < g<int>` is
/// `App(App(op_LessThan, TypeApp(f, [int])), TypeApp(g, [int]))` — the adjacent
/// `<`s open type applications, the spaced middle `<` is the comparison.
#[test]
fn diff_ast_type_app_then_comparison() {
    assert_asts_match("let x = f<int> < g<int>\n");
}

// ---- measured-constant disambiguation (codex review 2026-06) -------------

/// A unit-of-measure constant `1.0<ml>` shares the LexFilter's
/// `HighPrecedenceTyApp` marker with the type application (FCS:
/// `rawConstant HIGH_PRECEDENCE_TYAPP measureTypeArg`), but it is a measured
/// `SynConst`, **not** a `SynExpr.TypeApp`. The numeric-head guard in
/// `parse_postfix_tail` must decline it: no `TYPE_APP_EXPR` node is produced.
/// The measured-constant *expression* is not yet parsed (phase 10.8 was
/// type-side only), so this is a clean, lossless, non-panicking outcome — the
/// pre-10.20 behaviour, preserved (we assert that here rather than diff against
/// FCS, which produces `Const(Measure …)` we do not model).
#[test]
fn measured_float_constant_is_not_a_type_app() {
    for src in ["let x = 1.0<ml>\n", "let y = 5<m>\n", "let z = 1.0m<kg>\n"] {
        let parse = parse(src);
        assert_eq!(
            parse.root.text().to_string(),
            src,
            "round-trip must be lossless for the measured constant {src:?}",
        );
        assert!(
            !parse
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::TYPE_APP_EXPR),
            "measured constant {src:?} must not be parsed as a TYPE_APP_EXPR",
        );
    }
}

/// The guard is *numeric-head* specific: a member-generic application off a
/// dotted head (`obj.Method<int>`) still becomes a `TYPE_APP_EXPR` — the token
/// before the marker is the member ident, not a numeric literal.
#[test]
fn diff_ast_type_app_member_generic() {
    assert_asts_match("let x = Seq.length<int>\n");
}
