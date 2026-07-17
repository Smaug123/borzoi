//! Differential test (`parser::parse` vs FCS): type-annotated expressions and
//! the type-syntax surface they exercise. Split out of the former monolithic
//! `parser_diff.rs`.

use crate::common::{assert_asts_match, assert_asts_match_allow_errors};
use borzoi_cst::parser::parse;

/// Phase 7.1 — `(1 : int)`: typed-expression surface. FCS produces
/// `SynExpr.Paren(SynExpr.Typed(Const(Int32 1), SynType.LongIdent ["int"],
/// _), _, _, _)`; we wrap the inner `CONST_EXPR` in `TYPED_EXPR` via the
/// `parse_paren_expr` checkpoint hook and emit
/// `PAREN_EXPR > [LPAREN_TOK, TYPED_EXPR > [CONST_EXPR, COLON_TOK,
/// LONG_IDENT_TYPE > LONG_IDENT > IDENT_TOK("int")], RPAREN_TOK]`.
/// Pins the smallest typed-expression shape — the entry point for all
/// of phase 7.
#[test]
fn diff_ast_typed_int_in_paren() {
    assert_asts_match("(1 : int)\n");
}

/// Phase 7.1 — `(x : A.B.C)`: typed-expression with a dotted-path type.
/// FCS produces `SynType.LongIdent(SynLongIdent ["A"; "B"; "C"])`; the
/// projector mirrors `SynExpr.LongIdent` segment extraction so this
/// catches divergences in either side's path collection.
#[test]
fn diff_ast_typed_ident_with_long_ident_type() {
    assert_asts_match("(x : A.B.C)\n");
}

/// Phase 7.1 — `(x : _)`: wildcard type annotation. FCS projects to
/// `SynType.Anon(range)`; we emit `ANON_TYPE > UNDERSCORE_TOK` and both
/// sides reduce to `NormalisedType::Anon`.
#[test]
fn diff_ast_typed_ident_with_anon_type() {
    assert_asts_match("(x : _)\n");
}

/// Phase 7.1 — `(x : (int))`: parenthesised type. FCS produces
/// `SynType.Paren(SynType.LongIdent ["int"], _)`; we emit `PAREN_TYPE >
/// [LPAREN_TOK, LONG_IDENT_TYPE, RPAREN_TOK]`. Pins that the
/// `parse_atomic_type` LParen arm recurses into `parse_type` properly,
/// and that `bump_swallowed_rparen` works for type-level parens (since
/// LexFilter swallows `)` regardless of the enclosing construct).
#[test]
fn diff_ast_typed_ident_with_paren_type() {
    assert_asts_match("(x : (int))\n");
}

/// Phase 7.2 — `(x : 'a)`: plain quoted type variable. FCS produces
/// `SynType.Var(SynTypar("a", TyparStaticReq.None, false), _)`; we emit
/// `VAR_TYPE > [QUOTE_TOK, IDENT_TOK]` and both sides reduce to
/// `NormalisedType::Var { name: "a", head_type: false }`.
#[test]
fn diff_ast_typed_ident_with_quote_var_type() {
    assert_asts_match("(x : 'a)\n");
}

/// Phase 7.2 — `(x : ^T)`: head-typar (statically-resolved type
/// parameter). FCS recognises this via the `INFIX_AT_HAT_OP` rule with
/// the op text equalling `"^"`; we route a `Token::Op("^")` through
/// `parse_var_type` with [`SyntaxKind::HAT_TOK`]. Both sides reduce to
/// `NormalisedType::Var { name: "T", head_type: true }`.
#[test]
fn diff_ast_typed_ident_with_head_type_var_type() {
    assert_asts_match("(x : ^T)\n");
}

/// Phase 7.3 — `(f : int -> string)`: single function arrow. FCS
/// produces `SynType.Fun(LongIdent "int", LongIdent "string", _, _)`;
/// we emit `FUN_TYPE > [LONG_IDENT_TYPE, RARROW_TOK, LONG_IDENT_TYPE]`.
/// Both sides reduce to
/// `NormalisedType::Fun { arg: LongIdent ["int"], ret: LongIdent ["string"] }`.
#[test]
fn diff_ast_typed_ident_with_fun_type() {
    assert_asts_match("(f : int -> string)\n");
}

/// Phase 7.3 — `(f : int -> int -> int)`: chained arrows. FCS's `typ`
/// rule (`pars.fsy:6215`) is right-recursive, so this nests as
/// `Fun(int, Fun(int, int))`. Our parser mirrors that via the
/// right-recursive call in `parse_type` after `bump_into(RARROW_TOK)`.
#[test]
fn diff_ast_typed_ident_with_chained_fun_type() {
    assert_asts_match("(f : int -> int -> int)\n");
}

/// Phase 7.3 — `(f : 'a -> 'a)`: arrow over type variables. Composes
/// the 7.2 `VAR_TYPE` arms with the 7.3 arrow wrapping. Both sides
/// reduce to
/// `Fun { arg: Var "a" (head=false), ret: Var "a" (head=false) }`.
#[test]
fn diff_ast_typed_ident_with_fun_type_over_typars() {
    assert_asts_match("(f : 'a -> 'a)\n");
}

/// Phase 7.3 — `(f : (int -> int))`: arrow wrapped in parentheses. FCS
/// produces `Paren(Fun(int, int), _)`. Pins that `parse_atomic_type`'s
/// LPAREN arm recurses through `parse_type` (which now handles arrows),
/// and that an outer arrow is *not* introduced when the next non-trivia
/// raw is `)` rather than `->`.
#[test]
fn diff_ast_typed_ident_with_parenthesised_fun_type() {
    assert_asts_match("(f : (int -> int))\n");
}

/// Phase 7.4 — `(x : int * string)`: binary tuple type. FCS produces
/// `Tuple(false, [Type int; Star; Type string], _)`; we emit
/// `TUPLE_TYPE > [LONG_IDENT_TYPE, STAR_TOK, LONG_IDENT_TYPE]` and
/// both sides project to a `path` of three flat segments.
#[test]
fn diff_ast_typed_ident_with_tuple_type() {
    assert_asts_match("(x : int * string)\n");
}

/// Phase 7.4 — `(x : int * string * bool)`: ternary tuple, exercising
/// the flat-segment invariant. FCS produces a single `Tuple` whose
/// path is `[Type; Star; Type; Star; Type]`, *not* nested pairs; our
/// `parse_tuple_type` loop produces the same flat shape.
#[test]
fn diff_ast_typed_ident_with_ternary_tuple_type() {
    assert_asts_match("(x : int * string * bool)\n");
}

/// Phase 7.4 — `(f : int * int -> int)`: `*` binds tighter than `->`,
/// so FCS produces `Fun(Tuple(int, int), int, _, _)`. Pins our
/// precedence: the tuple layer sits *inside* the arrow layer, with the
/// LHS of `FUN_TYPE` being the `TUPLE_TYPE`.
#[test]
fn diff_ast_typed_ident_with_tuple_arg_to_fun_type() {
    assert_asts_match("(f : int * int -> int)\n");
}

/// Phase 7.4 — `(f : int -> int * int)`: arrow then tuple. Same
/// precedence rule, mirror side: FCS produces
/// `Fun(int, Tuple(int, int), _, _)`. Pins that the *return* type is
/// reached via `parse_type`'s recursive call, which itself goes
/// through `parse_tuple_type` before considering a further arrow.
#[test]
fn diff_ast_typed_ident_with_tuple_ret_in_fun_type() {
    assert_asts_match("(f : int -> int * int)\n");
}

/// Phase 7.4 — `(x : (int * int))`: parenthesised tuple. FCS produces
/// `Paren(Tuple(int, int), _)`. Pins that `parse_atomic_type`'s
/// LPAREN arm recurses into the full type grammar (including the new
/// tuple layer), and that no outer tuple is introduced when the next
/// non-trivia raw is `)` rather than `*`.
#[test]
fn diff_ast_typed_ident_with_parenthesised_tuple_type() {
    assert_asts_match("(x : (int * int))\n");
}

// Struct-tuple types — `struct (T1 * T2)` (FCS's `SynType.Tuple(isStruct =
// true, [Type; Star; Type], _)`). The `struct` keyword + parens are absorbed
// into a single flat `Tuple`; we emit `TUPLE_TYPE > [STRUCT_TOK, LPAREN_TOK,
// <segs>, RPAREN_TOK]` whose `segments()` skips the `struct`/parens, and
// `is_struct()` reads the marker. Motivated by `tasks.fsi`'s
// `Task<struct (^TResult1 * ^TResult2)>`.

/// A binary struct tuple — `struct (int * string)`.
#[test]
fn diff_ast_struct_tuple_binary() {
    assert_asts_match("(x : struct (int * string))\n");
}

/// A ternary struct tuple — exercises the flat-segment invariant under the
/// struct marker.
#[test]
fn diff_ast_struct_tuple_ternary() {
    assert_asts_match("(x : struct (int * string * bool))\n");
}

/// Head type-parameters as struct-tuple elements (`struct (^A * ^B)`) — the
/// SRTP shape from `tasks.fsi`.
#[test]
fn diff_ast_struct_tuple_head_typars() {
    assert_asts_match("let inline f (x: struct (^A * ^B)) = x\n");
}

/// A nested struct tuple as an element (`struct (int * struct (int * int))`).
#[test]
fn diff_ast_struct_tuple_nested() {
    assert_asts_match("(x : struct (int * struct (int * int)))\n");
}

/// Non-atomic elements — `struct (int list * (int -> int))` — confirming the
/// inner segments parse the full type grammar.
#[test]
fn diff_ast_struct_tuple_complex_elems() {
    assert_asts_match("(x : struct (int list * (int -> int)))\n");
}

/// A struct tuple as a *type argument* — the motivating `tasks.fsi` shape
/// `Task<struct (int * int)>`; pins that the unterminated-type-arg error is
/// gone once `struct (` is admitted at the `atomTypeOrAnonRecdType` head.
#[test]
fn diff_ast_struct_tuple_as_type_arg() {
    assert_asts_match("(x : System.Threading.Tasks.Task<struct (int * int)>)\n");
}

/// A struct tuple as an arrow LHS (`struct (int * int) -> int`) — confirms it
/// composes as an atomic type under the arrow layer.
#[test]
fn diff_ast_struct_tuple_arrow_lhs() {
    assert_asts_match("(f : struct (int * int) -> int)\n");
}

/// A struct tuple under a `#` flexible-type constraint (`#struct (int * int)`) —
/// FCS places `STRUCT LPAREN …` in `atomType`, the production `#` recurses into,
/// so the hash wraps the struct tuple (`HashConstraint(Tuple(isStruct))`).
#[test]
fn diff_ast_struct_tuple_under_hash() {
    assert_asts_match("(x : #struct (int * int))\n");
}

/// A `/` separator in the *tail* of a struct tuple (`struct (int * string / bool)`)
/// — the measure-style divisor, valid after the mandatory leading `*`.
#[test]
fn diff_ast_struct_tuple_slash_tail() {
    assert_asts_match("(x : struct (int * string / bool))\n");
}

/// A dot-path after a struct tuple (`struct (int * int).Nested`) — FCS admits a
/// struct tuple as an `atomType DOT path` LHS (like a `Paren`), wrapping it in a
/// `LongIdentApp`. Pins that the struct-tuple head flows into the dot-chain loop.
#[test]
fn diff_ast_struct_tuple_dot_chain() {
    assert_asts_match("(x : struct (int * int).Nested)\n");
}

/// Phase 7.5 — `(x : int list)`: minimal postfix application. FCS
/// produces `App(LongIdent list, None, [LongIdent int], [], None,
/// true, _)`; we emit `APP_TYPE > [LONG_IDENT_TYPE(int),
/// LONG_IDENT_TYPE(list)]` and both project to the same `App` shape
/// with `is_postfix = true`.
#[test]
fn diff_ast_typed_ident_with_postfix_app_type() {
    assert_asts_match("(x : int list)\n");
}

/// Phase 7.5 — `(x : int list option)`: pins left-associativity.
/// FCS's `appType appTypeConPower` is left-recursive, so the path
/// nests as `App(option, [App(list, [int])])` — the outer `App`'s
/// only type-arg is itself an `App`, not a flat list. Our
/// checkpoint-and-wrap loop in `parse_app_type` produces the same
/// nesting because every iteration's `start_node_at(cp, …)` wraps
/// the *previous* `APP_TYPE` as the first `Type` child of the new
/// outer node.
#[test]
fn diff_ast_typed_ident_with_chained_postfix_app_type() {
    assert_asts_match("(x : int list option)\n");
}

/// Phase 7.5 — `(x : int list * string list)`: pins app > tuple
/// precedence. FCS produces
/// `Tuple(false, [Type App(list, [int]); Star; Type App(list,
/// [string])], _)`; the two `App` nodes are tuple segments, not the
/// other way round. Our layering — `parse_tuple_type` calls
/// `parse_app_type` per segment — yields the same.
#[test]
fn diff_ast_typed_ident_with_tuple_of_postfix_app_types() {
    assert_asts_match("(x : int list * string list)\n");
}

/// Phase 7.5 — `(f : int -> int list)`: pins app > arrow precedence.
/// FCS produces `Fun(int, App(list, [int]), _, _)`; the *return*
/// type is the `App`, not an arrow wrapping it. Our recursive
/// `parse_type` call for the return goes through `parse_tuple_type
/// → parse_app_type` before any further arrow check, so the inner
/// nesting matches.
#[test]
fn diff_ast_typed_ident_with_postfix_app_return_in_fun_type() {
    assert_asts_match("(f : int -> int list)\n");
}

/// Phase 7.5 — `(x : (int -> int) list)`: postfix application
/// whose argument is itself a parenthesised arrow. FCS produces
/// `App(list, [Paren(Fun(int, int))], _)`; our `parse_atomic_type`
/// LPAREN arm recurses into `parse_type` and yields the same
/// `Paren` arg before the postfix loop wraps it as `App`.
#[test]
fn diff_ast_typed_ident_with_postfix_app_over_parenthesised_fun_type() {
    assert_asts_match("(x : (int -> int) list)\n");
}

/// Phase 7.5 — `(x : 'a list)`: typar argument. FCS produces
/// `App(list, [Var('a)], _)`; pins that `parse_atomic_type`'s
/// QUOTE_TOK arm composes with the postfix loop (the typar is the
/// arg, the longident is the head).
#[test]
fn diff_ast_typed_ident_with_typar_arg_postfix_app_type() {
    assert_asts_match("(x : 'a list)\n");
}

/// Phase 7.6 — `(x : List<int>)`: minimal prefix application. FCS
/// produces `App(LongIdent List, Some lessRange, [LongIdent int],
/// [], Some greaterRange, false, _)`; we emit `APP_TYPE >
/// [LONG_IDENT_TYPE(List), ERROR(HPA), LESS_TOK,
/// LONG_IDENT_TYPE(int), GREATER_TOK]` and both project to the same
/// `App` shape with `is_postfix = false`. Drives the LexFilter HPA
/// virtual + `Less(true)` / `Greater(true)` typar-bracket promotion.
#[test]
fn diff_ast_typed_ident_with_prefix_app_type() {
    assert_asts_match("(x : List<int>)\n");
}

/// Phase 7.6 — `(x : Foo< >)`: empty type-arg list. FCS's
/// `typeArgsActual` has a `LESS GREATER` arm (`pars.fsy:6649`) yielding
/// `App(Foo, Some _, [], [], Some _, false, _)` with no parse error.
/// The space is load-bearing: adjacent `<>` lexes as the `<>` inequality
/// operator, which FCS rejects in type position.
#[test]
fn diff_ast_typed_ident_with_empty_prefix_app_type() {
    assert_asts_match("(x : Foo< >)\n");
}

/// Phase 7.6 — `(x : Dictionary<string, int>)`: pins the
/// multi-arg, comma-separated prefix form. FCS produces
/// `App(Dictionary, _, [LongIdent string; LongIdent int], [comma],
/// _, false, _)`; our `parse_app_type` loop on `Token::Comma` emits
/// one `COMMA_TOK` between two parsed type args.
#[test]
fn diff_ast_typed_ident_with_multi_arg_prefix_app_type() {
    assert_asts_match("(x : Dictionary<string, int>)\n");
}

/// Phase 7.6 — `(x : List<List<int>>)`: nested generics with a
/// trailing `>>` that LexFilter's `smash_typar_token` splits into
/// two separate `>` tokens. FCS nests as `App(List, [App(List,
/// [int])], false)`; our parser bumps the first `>` to close the
/// inner APP_TYPE and the second to close the outer.
#[test]
fn diff_ast_typed_ident_with_nested_prefix_app_type() {
    assert_asts_match("(x : List<List<int>>)\n");
}

/// Phase 7.6 — `(x : Foo<int> list)`: mixed prefix + postfix on the
/// same path. FCS produces `App(list, [App(Foo, [int], false)],
/// true)` — outer postfix wraps inner prefix. Our `parse_app_type`
/// runs the prefix branch first, then falls through to the postfix
/// loop, with both wraps starting at the same `cp` so the inner
/// APP_TYPE becomes the outer's only Type-arg.
#[test]
fn diff_ast_typed_ident_with_postfix_app_of_prefix_app() {
    assert_asts_match("(x : Foo<int> list)\n");
}

/// Phase 7.6 — `(x : List<int -> int>)`: pins that `typeArgActual`
/// admits a full `typ` (including arrows). FCS produces
/// `App(List, [Fun(int, int)], false)`; our prefix branch calls
/// `parse_type` per arg so the inner arrow nests inside the
/// brackets.
#[test]
fn diff_ast_typed_ident_with_prefix_app_type_function_arg() {
    assert_asts_match("(x : List<int -> int>)\n");
}

/// Phase 7.6 — `(x : Foo<^T>)`: SRTP first arg after a fused `<^`
/// opener. The raw lexer tokenises `<^` as a single `Op("<^")`;
/// LexFilter's `smash_typar_token` splits it into filtered
/// `Less(true) + Op("^")`. After bumping `LESS_TOK`, `raw_pos`
/// still points at the unsplit fused raw, so `parse_type`'s
/// raw-stream gate (`next_non_trivia_raw_at_pos`) must skip the
/// already-partly-consumed raw via the `raw_consumed_end` check.
/// FCS produces `App(Foo, [Var(^T, HeadType)], false)`.
#[test]
fn diff_ast_typed_ident_with_prefix_app_type_srtp_first_arg() {
    assert_asts_match("(x : Foo<^T>)\n");
}

/// Phase 7.6 — `(x : List<List<int>> option)`: nested generics
/// whose split `>>` close is followed by a postfix-app head. After
/// the inner `>` is bumped (first half of the `>>` split), the
/// raw-stream lookahead must return the *second* `>` (the upcoming
/// filtered split tail), not skip past it to `option`. Otherwise
/// the inner-level postfix loop would fire while the filtered
/// cursor is still on `Greater`, and `parse_atomic_type` would
/// panic on a non-type-starter. FCS produces
/// `App(option, [App(List, [App(List, [int])])], true)`.
#[test]
fn diff_ast_typed_ident_with_nested_prefix_app_followed_by_postfix() {
    assert_asts_match("(x : List<List<int>> option)\n");
}

/// Spaced / deprecated prefix app — `(x : List < int >)`. FCS's
/// `appTypeCon typeArgsNoHpaDeprecated` (`pars.fsy:6596`) accepts the bare
/// (non-HPA) `typeArgsActual` arm with warning FS1190, building the same
/// `App(List, [int], false)` as the adjacent `List<int>` form. Our parser
/// has no warning channel, so it produces the identical `APP_TYPE` with no
/// error — same shape as `diff_ast_typed_ident_with_prefix_app_type`.
#[test]
fn diff_ast_typed_ident_with_spaced_prefix_app_type() {
    assert_asts_match("(x : List < int >)\n");
}

/// Spaced prefix app over a dotted path — `(x : System.Collections < int >)`.
/// The dotted head is one greedy `appTypeCon`, so the spaced `< int >`
/// attaches at the same prefix-app site as the non-dotted form.
#[test]
fn diff_ast_typed_ident_with_spaced_dotted_prefix_app_type() {
    assert_asts_match("(x : System.Collections < int >)\n");
}

/// Spaced multi-arg prefix app — `(x : Dictionary < string , int >)`.
#[test]
fn diff_ast_typed_ident_with_spaced_multi_arg_prefix_app_type() {
    assert_asts_match("(x : Dictionary < string , int >)\n");
}

/// Spaced nested generics — `(x : List < List < int > >)`. Mirrors the
/// adjacent `(x : List<List<int>>)` nest, but every `<`/`>` is a bare
/// (non-HPA) typar bracket rather than a LexFilter-smashed one.
#[test]
fn diff_ast_typed_ident_with_spaced_nested_prefix_app_type() {
    assert_asts_match("(x : List < List < int > >)\n");
}

/// Spaced prefix + postfix mix — `(x : Foo < int > list)`. The spaced
/// prefix app builds first, then the postfix `list` wraps it, exactly as
/// the adjacent `(x : Foo<int> list)` form does.
#[test]
fn diff_ast_typed_ident_with_spaced_prefix_then_postfix_app() {
    assert_asts_match("(x : Foo < int > list)\n");
}

/// Phase 7.7 — `(x : int[])`: the minimal array type. FCS's grammar
/// (`pars.fsy:6371`) has two array-suffix arms — one bare `LBRACK`
/// after a paren head, one `HIGH_PRECEDENCE_BRACK_APP LBRACK` after
/// an IDENT head. The IDENT-adjacent form fires here: LexFilter
/// emits a zero-width `HighPrecedenceBrackApp` virtual between the
/// IDENT `int` and the `[`, and the array-suffix loop swallows it
/// before bumping `LBRACK_TOK`. FCS projects to
/// `SynType.Array(1, LongIdent int, _)`.
#[test]
fn diff_ast_typed_ident_with_array_type() {
    assert_asts_match("(x : int[])\n");
}

/// Phase 7.7 — `(x : int[,])`: rank-2 array. The single comma
/// between the brackets contributes to `rank = 1 + commas`. FCS
/// projects to `SynType.Array(2, LongIdent int, _)`.
#[test]
fn diff_ast_typed_ident_with_array_type_rank_two() {
    assert_asts_match("(x : int[,])\n");
}

/// Phase 7.7 — `(x : int[,,])`: rank-3 array. Two commas yield
/// `rank = 3`. FCS projects to `SynType.Array(3, LongIdent int, _)`.
#[test]
fn diff_ast_typed_ident_with_array_type_rank_three() {
    assert_asts_match("(x : int[,,])\n");
}

/// Phase 7.7 — `(x : int[][])`: jagged array (left-associative
/// chaining). FCS nests as `Array(1, Array(1, int))`; the outer
/// suffix's element type is itself an `Array`. Our shared rowan
/// checkpoint `cp` in `parse_app_type` produces the same nesting
/// because the second `start_node_at(cp, ARRAY_TYPE)` wraps the
/// first array node as the new outer's first child.
#[test]
fn diff_ast_typed_ident_with_jagged_array_type() {
    assert_asts_match("(x : int[][])\n");
}

/// Phase 7.7 — `(x : int list[])`: array suffix applied to a
/// postfix-app head. FCS produces `Array(1, App(list, [int]), _)`;
/// the unified postfix-app + array-suffix loop in `parse_app_type`
/// runs the postfix wrap first (`int list`), then the array-suffix
/// wrap, so the `ARRAY_TYPE` ends up outermost with the `APP_TYPE`
/// as its element child.
#[test]
fn diff_ast_typed_ident_with_postfix_app_under_array() {
    assert_asts_match("(x : int list[])\n");
}

/// Phase 7.7 — `(x : (int)[])`: array suffix after a parenthesised
/// head. FCS's bare-LBRACK array-suffix arm fires here (no
/// `HighPrecedenceBrackApp` virtual, because LexFilter only emits
/// HPBA adjacent to an IDENT, not after `)`). Our parser sees no
/// HPBA virtual at the start of the array-suffix block and skips the
/// optional ERROR placeholder; the resulting tree omits the
/// zero-width ERROR child but still projects to
/// `Array(1, Paren(LongIdent int), _)`.
#[test]
fn diff_ast_typed_ident_with_paren_head_array() {
    assert_asts_match("(x : (int)[])\n");
}

/// Phase 7.7 — `(x : (int -> int)[])`: array of function type.
/// Pins that the array-suffix loop sits *outside* the parenthesised
/// arrow — FCS produces `Array(1, Paren(Fun(int, int)), _)`. Our
/// `parse_atomic_type` LPAREN arm recurses into `parse_type` for the
/// arrow, returns the `PAREN_TYPE`, and the outer `parse_app_type`
/// then wraps it in `ARRAY_TYPE`.
#[test]
fn diff_ast_typed_ident_with_function_under_array() {
    assert_asts_match("(x : (int -> int)[])\n");
}

/// Phase 7.8 — `(x : #int)`: basic hash-constraint (flexible type).
/// FCS's `hashConstraint: HASH atomType` (`pars.fsy:2609-2611`)
/// projects to `SynType.HashConstraint(LongIdent int, _)`.
#[test]
fn diff_ast_typed_ident_with_hash_constraint() {
    assert_asts_match("(x : #int)\n");
}

/// Phase 7.8 — `(x : ##int)`: nested hash. The inner
/// `parse_atomic_type` recurses into another hash branch; FCS
/// nests as `HashConstraint(HashConstraint(int))`.
#[test]
fn diff_ast_typed_ident_with_nested_hash_constraint() {
    assert_asts_match("(x : ##int)\n");
}

/// Phase 7.8 — `(x : #'T)`: hash over a type variable. FCS projects
/// the inner to a `SynType.Var`, so the result is
/// `HashConstraint(Var 'T)`.
#[test]
fn diff_ast_typed_ident_with_hash_constraint_over_typar() {
    assert_asts_match("(x : #'T)\n");
}

/// Phase 7.8 — `(x : #Foo<int>)`: hash over a prefix-app. Pins the
/// FCS layering `atomType: hashConstraint | appTypeConPower`
/// (`pars.fsy:6534-6549`): the prefix-app sits *under* the hash, so
/// FCS produces `HashConstraint(App(Foo, [int]))`. Our parser keeps
/// the HPA wrap in `parse_atomic_type` (above the hash branch's
/// recursion) precisely to preserve this nesting.
#[test]
fn diff_ast_typed_ident_with_hash_constraint_over_prefix_app() {
    assert_asts_match("(x : #Foo<int>)\n");
}

/// Phase 7.8 — `(x : #(int -> int))`: hash over a parenthesised
/// arrow. `LPAREN typ rparen` is an `atomType`, so the inner can be
/// `Paren(Fun(int, int))`. FCS produces
/// `HashConstraint(Paren(Fun(int, int)))`.
#[test]
fn diff_ast_typed_ident_with_hash_constraint_over_paren_arrow() {
    assert_asts_match("(x : #(int -> int))\n");
}

/// Phase 7.8 — `(x : #int list)`: postfix-app outside the hash.
/// The hash branch returns from `parse_atomic_type` after `#int`;
/// `parse_app_type`'s postfix loop then wraps that hash node in
/// `APP_TYPE` from its shared checkpoint. FCS produces
/// `App(list, [HashConstraint(int)], postfix)`.
#[test]
fn diff_ast_typed_ident_with_postfix_app_over_hash_constraint() {
    assert_asts_match("(x : #int list)\n");
}

/// Phase 7.9 — `(x : {| F : int |})`: single-field anon-record type.
/// FCS's `anonRecdType: braceBarFieldDeclListCore`
/// (`pars.fsy:2515-2522`) projects to
/// `SynType.AnonRecd(isStruct: false, fields: [(F, int)], _)`.
#[test]
fn diff_ast_typed_ident_with_anon_recd_type_single_field() {
    assert_asts_match("(x : {| F : int |})\n");
}

/// Phase 7.9 — `(x : {| F : int; G : string |})`: multi-field
/// anon-record. FCS's `recdFieldDeclList` consumes the `;` between
/// fields and emits `[(F, int); (G, string)]`.
#[test]
fn diff_ast_typed_ident_with_anon_recd_type_multiple_fields() {
    assert_asts_match("(x : {| F : int; G : string |})\n");
}

/// Phase 7.9 — `(x : struct {| F : int |})`: struct variant. FCS's
/// `anonRecdType: STRUCT braceBarFieldDeclListCore`
/// (`pars.fsy:2510-2513`) sets `isStruct = true`.
#[test]
fn diff_ast_typed_ident_with_anon_recd_type_struct_variant() {
    assert_asts_match("(x : struct {| F : int |})\n");
}

/// Phase 7.9 — `(x : {| F : int -> int |})`: the field type is a
/// full `typ` (FCS's `recdFieldDecl: opt_mutable opt_access ident COLON
/// typ`, `pars.fsy:2978-2980`), so a `Fun` projects under the field
/// without parens.
#[test]
fn diff_ast_typed_ident_with_anon_recd_type_inner_function_type() {
    assert_asts_match("(x : {| F : int -> int |})\n");
}

/// Phase 7.9 — `(x : {| F : int |} list)`: anon-recd under a
/// postfix application. FCS's
/// `appType: atomTypeOrAnonRecdType (appTypeConPower)+`
/// (`pars.fsy:6378`) wraps the anon-recd as the sole arg of
/// `App(list, …, postfix)`.
#[test]
fn diff_ast_typed_ident_with_postfix_app_over_anon_recd_type() {
    assert_asts_match("(x : {| F : int |} list)\n");
}

/// Phase 7.9 — `(x : {| F : int |}[])`: anon-recd under an array
/// suffix. The parser's shared `parse_app_type` checkpoint wraps the
/// anon-recd in `ARRAY_TYPE` the same way it does for other heads.
#[test]
fn diff_ast_typed_ident_with_array_over_anon_recd_type() {
    assert_asts_match("(x : {| F : int |}[])\n");
}

/// Phase 7.10 — `(x : (int).Foo)`: parenthesised-type root, single
/// path segment. The minimal LongIdentApp surface FCS accepts.
/// `atomType DOT path %prec prec_atomtyp_get_path`
/// (`pars.fsy:6600-6601`) projects to
/// `SynType.LongIdentApp(Paren(int), [Foo], None, [], [], None, _)`.
///
/// NB: a bare typar root (`'T.Foo`) is grammatically admissible per
/// `pars.fsy:6600` but is rejected by FCS's compiled LR tables, so the
/// dot-chain loop only fires when the LHS is Paren / Anon / HPA-wrapped
/// App — see `parse_atomic_type`'s `head_can_chain` gate.
#[test]
fn diff_ast_typed_ident_with_long_ident_app_paren_int_root() {
    assert_asts_match("(x : (int).Foo)\n");
}

/// Phase 7.10 — `(x : (int).Foo<string>)`: LongIdentApp with explicit
/// `typeArgsNoHpaDeprecated`. The HPA-prefixed `LESS … GREATER`
/// follows the path; FCS projects to
/// `SynType.LongIdentApp(Paren(int), [Foo], Some _, [LongIdent string], [], Some _, _)`.
#[test]
fn diff_ast_typed_ident_with_long_ident_app_paren_root_with_args() {
    assert_asts_match("(x : (int).Foo<string>)\n");
}

/// Phase 7.10 — `(x : (int).Foo< >)`: LongIdentApp with an *empty*
/// `typeArgsNoHpaDeprecated`. FCS's `typeArgsActual: LESS GREATER` arm
/// (`pars.fsy:6649`) produces zero args without error, so this projects
/// to `LongIdentApp(Paren(int), [Foo], Some _, [], [], Some _, _)`. The
/// space matters: adjacent `<>` lexes as the `<>` inequality operator.
#[test]
fn diff_ast_typed_ident_with_long_ident_app_empty_args() {
    assert_asts_match("(x : (int).Foo< >)\n");
}

/// Phase 7.10 — `(x : (int).Foo.Bar)`: multi-segment dotted path on the
/// right of a non-`path` head. FCS's `path` is itself a dotted
/// long-ident, so this projects to one
/// `LongIdentApp(Paren(int), [Foo; Bar], …)` rather than two nested
/// LongIdentApps.
#[test]
fn diff_ast_typed_ident_with_long_ident_app_multi_segment_path() {
    assert_asts_match("(x : (int).Foo.Bar)\n");
}

/// Phase 7.10 — `(x : Foo<int>.Bar)`: prefix-app head retro-wrapped
/// by the dot-chain. The HPA-prefix wrap lifts `Foo` into
/// `App(Foo, [int])`; the `.Bar` then wraps that as the root of a
/// `LongIdentApp(App(Foo, [int]), [Bar], …)`. Pins the layering
/// against the alternative `Foo.Bar<int>` which is plain
/// `App(LongIdent Foo.Bar, [int])`.
#[test]
fn diff_ast_typed_ident_with_long_ident_app_app_prefix_root() {
    assert_asts_match("(x : Foo<int>.Bar)\n");
}

/// Phase 7.10 — `(x : (int list).Foo)`: parenthesised type as the
/// LongIdentApp root. FCS's `atomType: LPAREN typ rparen` is admissible
/// as the LHS, so the projection is
/// `LongIdentApp(Paren(App(list, [int], postfix)), [Foo], …)`.
#[test]
fn diff_ast_typed_ident_with_long_ident_app_paren_root() {
    assert_asts_match("(x : (int list).Foo)\n");
}

/// Phase 7.10 — `(x : (int).Foo<string>.Bar)`: chained LongIdentApp.
/// After the first iteration of the dot-chain loop produces
/// `LongIdentApp(Paren(int), [Foo], Some _, [string], …)`, the second
/// iteration retro-wraps that node as the root of an outer
/// `LongIdentApp(_, [Bar], None, …)`. Pins left-associative nesting.
#[test]
fn diff_ast_typed_ident_with_long_ident_app_chained() {
    assert_asts_match("(x : (int).Foo<string>.Bar)\n");
}

/// Phase 7.10 — `(x : (int).Foo list)`: postfix-app outside the
/// LongIdentApp. The dot-chain loop in `parse_atomic_type` returns;
/// `parse_app_type`'s postfix loop then wraps the LongIdentApp as
/// the sole arg of an outer `App(list, …, postfix)`. FCS produces
/// `App(list, [LongIdentApp(Paren(int), [Foo], …)], postfix)`.
#[test]
fn diff_ast_typed_ident_with_postfix_app_over_long_ident_app() {
    assert_asts_match("(x : (int).Foo list)\n");
}

/// Phase 7.11 — `(x : int list | null)`: the postfix `list` binds
/// inside the nullable wrap (`appTypeWithoutNull` already includes the
/// postfix application). FCS produces
/// `WithNull(App(list, [int], postfix), …)`.
#[test]
fn diff_ast_typed_ident_with_with_null_over_postfix_app() {
    assert_asts_match("(x : int list | null)\n");
}

/// Phase 7.11 — `(x : string | null * int)`: the nullable binds
/// tighter than the tuple `*` (`tupleType: appTypeCanBeNullable STAR
/// …`), so FCS produces `Tuple([WithNull(string); int])`, not
/// `WithNull(string, Tuple(null * int))`.
#[test]
fn diff_ast_typed_ident_with_with_null_in_tuple() {
    assert_asts_match("(x : string | null * int)\n");
}

/// Phase 7.11 — `(x : string | null -> int)`: the nullable binds
/// tighter than the arrow (`typ: tupleType RARROW typ`), so the
/// function argument is `WithNull(string)`: FCS produces
/// `Fun(WithNull(string), int)`.
#[test]
fn diff_ast_typed_ident_with_with_null_arrow() {
    assert_asts_match("(x : string | null -> int)\n");
}

/// Phase 7.11 — `(x : Foo<string | null>)`: a nullable type as a
/// generic type-argument. The type-arg list is `typ`, which reaches
/// `appTypeCanBeNullable`, so FCS admits `string | null` inside the
/// `<…>` as `App(Foo, [WithNull(string)])`.
#[test]
fn diff_ast_typed_ident_with_with_null_generic_arg() {
    assert_asts_match("(x : Foo<string | null>)\n");
}

// ---------------------------------------------------------------------------
// Phase 10.8 — units of measure (`SynType.MeasurePower` + `SynRationalConst`).
//
// Reached through the prefix-app `<…>` type-argument surface
// (`float<measure>`). The measure-power production is
// `appTypeCon INFIX_AT_HAT_OP atomicRationalConstant` (`pars.fsy:6344`); the
// rational-constant grammar is `pars.fsy:3483-3515`. The `/` measure-division
// form (`float<1/s>`) and the dimensionless `float<1>` are deferred to 10.9
// (they need `SynType.StaticConstant` + the `SynTupleTypeSegment.Slash`
// segment). Shapes ground-truthed with `dotnet tools/fcs-dump ast`.

/// `m^2` → `App(float, [MeasurePower(LongIdent m, Integer 2)])`. The
/// minimal measure power: a `^` operator and a bare integer exponent.
#[test]
fn diff_ast_measure_power_int() {
    assert_asts_match("(x : float<m^2>)\n");
}

/// `m^-1` → `MeasurePower(m, Negate(Integer 1))`. The `^-` lexes as a
/// single `INFIX_AT_HAT_OP` token whose text is `"^-"`; FCS wraps the
/// exponent in `Negate` when the operator carries the trailing minus.
#[test]
fn diff_ast_measure_power_negate() {
    assert_asts_match("(x : float<m^-1>)\n");
}

/// `m^ -2` → `MeasurePower(m, Integer -2)`. The space-separated `-2`
/// folds into a single negative literal (`sign_fold`, since `^` is not
/// an atomic-expr-end), so the exponent is `Integer(-2)`, *not*
/// `Negate(Integer 2)`.
#[test]
fn diff_ast_measure_power_spaced_negative_literal() {
    assert_asts_match("(x : float<m^ -2>)\n");
}

/// `m^(1/2)` → `MeasurePower(m, Paren(Rational(1, 2)))`. The rational
/// exponent `1/2` is reachable only inside the parenthesised
/// `rationalConstant`.
#[test]
fn diff_ast_measure_power_paren_rational() {
    assert_asts_match("(x : float<m^(1/2)>)\n");
}

/// `m^(2)` → `MeasurePower(m, Paren(Integer 2))`. A parenthesised plain
/// integer exponent.
#[test]
fn diff_ast_measure_power_paren_int() {
    assert_asts_match("(x : float<m^(2)>)\n");
}

/// `m^(-1)` → `MeasurePower(m, Paren(Integer -1))`. Inside the parens the
/// adjacent `-1` folds into a single negative literal, so it is
/// `Integer(-1)` rather than `Negate(Integer 1)`.
#[test]
fn diff_ast_measure_power_paren_negative_literal() {
    assert_asts_match("(x : float<m^(-1)>)\n");
}

/// `m^(- 2)` → `MeasurePower(m, Paren(Negate(Integer 2)))`. The
/// space-separated `-` does *not* fold, so it is a real `MINUS` →
/// `Negate(Integer 2)`.
#[test]
fn diff_ast_measure_power_paren_spaced_negate() {
    assert_asts_match("(x : float<m^(- 2)>)\n");
}

/// `'a^2` → `MeasurePower(Var 'a, Integer 2)`. The base of a measure
/// power may be a type variable, not only a `LongIdent`.
#[test]
fn diff_ast_measure_power_typar_base() {
    assert_asts_match("(x : float<'a^2>)\n");
}

/// `kg m` → `App(m, [kg], isPostfix=true)`. A measure *product* is just
/// juxtaposition, which FCS models with the ordinary postfix
/// type-application — so this exercises the existing phase-7 postfix
/// loop with no new machinery (pinned here to guard that reuse).
#[test]
fn diff_ast_measure_product() {
    assert_asts_match("(x : float<kg m>)\n");
}

/// `kg m^2` → `App(MeasurePower(m, 2), [kg], isPostfix=true)`. A product
/// whose right factor is itself a measure power: the postfix loop's
/// right-hand head (`parse_app_type_con_power`) absorbs the `^2`.
#[test]
fn diff_ast_measure_product_over_power() {
    assert_asts_match("(x : float<kg m^2>)\n");
}

/// `(m)^2` → `MeasurePower(Paren(m), Integer 2)`. FCS's `powerType` base is an
/// `atomTypeOrAnonRecdType`, so a *parenthesised* base is valid; the
/// measure-power tail is detected on the head atom in `parse_app_type` (not
/// only the path/typar `parse_app_type_con_power`).
#[test]
fn diff_ast_measure_power_paren_base() {
    assert_asts_match("(x : float<(m)^2>)\n");
}

/// `(kg m)^2` → `MeasurePower(Paren(App(m, [kg], postfix)), Integer 2)`. A
/// parenthesised *product* as the power base.
#[test]
fn diff_ast_measure_power_paren_product_base() {
    assert_asts_match("(x : float<(kg m)^2>)\n");
}

/// `m^0x2` → `MeasurePower(m, Integer 2)`. A hex exponent: FCS's measure
/// exponent `INT32` token includes hex/oct/bin literals (decoded two's
/// complement), not only decimal.
#[test]
fn diff_ast_measure_power_hex_exponent() {
    assert_asts_match("(x : float<m^0x2>)\n");
}

/// `m^0o5` → `MeasurePower(m, Integer 5)`. An octal exponent.
#[test]
fn diff_ast_measure_power_oct_exponent() {
    assert_asts_match("(x : float<m^0o5>)\n");
}

/// `m^(1l/2l)` → `MeasurePower(m, Paren(Rational(1, 2)))`. A lowercase-`l`
/// suffixed Int32 numerator/denominator — FCS classifies these as the same
/// `INT32` terminal (an uppercase-`L` Int64 / `uy` byte suffix is rejected).
#[test]
fn diff_ast_measure_power_lsuffix_rational() {
    assert_asts_match("(x : float<m^(1l/2l)>)\n");
}

/// `(x : m^(1)) / y` → the parenthesised exponent `(1)` must **not** swallow
/// the outer `/`. LexFilter removes the `)` that closes the exponent paren
/// (and the `(x : …)` paren) from the filtered stream, so a filtered-only
/// divisor lookahead would mis-read `(1) / y` as the rational `1/…`; the raw
/// gate keeps the exponent as `Paren(Integer 1)` and leaves `/ y` to the outer
/// division. FCS parses cleanly (no errors).
#[test]
fn diff_ast_measure_power_paren_exponent_then_outer_slash() {
    assert_asts_match("let z = (x : m^(1)) / y\n");
}

/// `m^(1/0)` → `MeasurePower(m, Paren(Rational(1, 0)))` *with* a parse error.
/// FCS reports `parsIllegalDenominatorForMeasureExponent` (a zero
/// denominator) but still builds the `Rational(1, 0)` node, so the AST shape
/// matches while both sides carry an error.
#[test]
fn diff_ast_measure_power_zero_denominator_is_error() {
    assert_asts_match_allow_errors("(x : float<m^(1/0)>)\n");
}

// ---------------------------------------------------------------------------
// Phase 10.9 — type-provider static arguments (`SynType.StaticConstant*`).
//
// FCS's `atomicType` (`pars.fsy:6575-6589`) admits a bare literal
// (`rawConstant` / `TRUE` / `FALSE`) as `StaticConstant`, `NULL` as
// `StaticConstantNull`, and `CONST atomicExpr` as `StaticConstantExpr`; the
// type-argument-actual level (`typeArgActual: typ EQUALS typ`,
// `pars.fsy:6668`) adds the named `StaticConstantNamed`. These typecheck only
// as type-provider static arguments, but the *grammar* accepts them anywhere a
// type appears, so the bare forms `(x : 42)` / `(x : null)` parse too (matching
// FCS). The dimensionless `float<1>` is a plain `StaticConstant`, and the `/`
// measure division `float<1/s>` is a `Tuple` whose path carries a
// `SynTupleTypeSegment.Slash` (`pars.fsy:6262-6285`). Shapes ground-truthed
// with `dotnet tools/fcs-dump ast`.

/// `Foo<"literal">` → `App(Foo, [StaticConstant(String "literal")])`. The
/// minimal static-arg form: a string literal as a type-provider argument.
#[test]
fn diff_ast_static_const_string_arg() {
    assert_asts_match("(x : Foo<\"literal\">)\n");
}

/// `Foo<42>` → `App(Foo, [StaticConstant(Int32 42)])`. An integer literal arg.
#[test]
fn diff_ast_static_const_int_arg() {
    assert_asts_match("(x : Foo<42>)\n");
}

/// `Foo<true>` → `App(Foo, [StaticConstant(Bool true)])`. FCS routes `TRUE`
/// through a dedicated `atomicType` arm (not `rawConstant`); our parser shares
/// the `parse_const_payload` `BOOL_LIT` path, so both reduce to the same
/// `SynConst.Bool`.
#[test]
fn diff_ast_static_const_bool_arg() {
    assert_asts_match("(x : Foo<true>)\n");
}

/// `Foo<null>` → `App(Foo, [StaticConstantNull])`. The payload-less null arg.
#[test]
fn diff_ast_static_const_null_arg() {
    assert_asts_match("(x : Foo<null>)\n");
}

/// `Foo<__LINE__>` → `App(Foo, [StaticConstant(SourceIdentifier "__LINE__")])`.
/// A source-identifier keyword-string as a static argument: FCS routes it
/// through `rawConstant`'s `sourceIdentifier` arm, and our parser reaches it
/// via `raw_starts_const_payload` → `parse_const_payload`. Guards the
/// static-constant-type consumer of that predicate (the same fix that admits
/// the keyword-string in pattern position).
#[test]
fn diff_ast_static_const_source_identifier_arg() {
    assert_asts_match("(x : Foo<__LINE__>)\n");
}

/// `Foo<__SOURCE_FILE__>` follows the same static-constant route, but carries a
/// path-valued source identifier.
#[test]
fn diff_ast_static_const_source_file_arg() {
    assert_asts_match("(x : Foo<__SOURCE_FILE__>)\n");
}

/// Same static-constant type path with `__LINE__` on line 2, proving the
/// normaliser compares the expanded line value in type position too.
#[test]
fn diff_ast_static_const_line_value_on_second_line() {
    assert_asts_match("let _ =\n    (x : Foo<__LINE__>)\n");
}

/// `Foo<const E>` → `App(Foo, [StaticConstantExpr(Ident E)])`. The `const`
/// keyword introduces an atomic *expression* as the static argument.
#[test]
fn diff_ast_static_const_expr_ident_arg() {
    assert_asts_match("(x : Foo<const E>)\n");
}

/// `Foo<const 5>` → `App(Foo, [StaticConstantExpr(Const(Int32 5))])`. A literal
/// const-expression argument (the inner atomic expr is a `SynExpr.Const`, not
/// the bare `StaticConstant` the un-`const`'d `Foo<5>` would produce).
#[test]
fn diff_ast_static_const_expr_literal_arg() {
    assert_asts_match("(x : Foo<const 5>)\n");
}

/// `Foo<const -1>` → `App(Foo, [StaticConstantExpr(Const(Int32 -1))])`. The
/// `atomicExpr` after `const` may be a sign-folded negative literal: its
/// filtered token is the merged `INT32_LIT("-1")` while the raw cursor is the
/// pre-fold `Op("-")`, so the const-expr gate must admit the fold.
#[test]
fn diff_ast_static_const_expr_negative_literal_arg() {
    assert_asts_match("(x : Foo<const -1>)\n");
}

/// `Foo<const (1)>` → `App(Foo, [StaticConstantExpr(Paren(Const(Int32 1)))])`.
/// A parenthesised atomic expression as the `const` argument.
#[test]
fn diff_ast_static_const_expr_paren_arg() {
    assert_asts_match("(x : Foo<const (1)>)\n");
}

/// `Foo<N=42>` → `App(Foo, [StaticConstantNamed(LongIdent N, StaticConstant
/// 42)])`. The named form `typ EQUALS typ`; the value `42` is itself a
/// `StaticConstant` because it is a type-arg-actual.
#[test]
fn diff_ast_static_const_named_int_value() {
    assert_asts_match("(x : Foo<N=42>)\n");
}

/// `Foo<N=int>` → `App(Foo, [StaticConstantNamed(LongIdent N, LongIdent
/// int)])`. The named form whose value is an ordinary type, not a static
/// constant — confirming both sides of `StaticConstantNamed` are full `typ`s.
#[test]
fn diff_ast_static_const_named_type_value() {
    assert_asts_match("(x : Foo<N=int>)\n");
}

/// `Foo<42, 43>` → `App(Foo, [StaticConstant 42; StaticConstant 43])`. Two
/// static-const args separated by the type-arg comma.
#[test]
fn diff_ast_static_const_multiple_args() {
    assert_asts_match("(x : Foo<42, 43>)\n");
}

/// `(x : 42)` → `StaticConstant(Int32 42)` outside any `<…>` surface. FCS's
/// `atomicType` admits a bare literal anywhere a type is expected, so the bare
/// annotation parses cleanly (the static-const arm lives in `parse_atomic_type`,
/// not only the type-arg loop).
#[test]
fn diff_ast_static_const_bare_int() {
    assert_asts_match("(x : 42)\n");
}

/// `(x : null)` → `StaticConstantNull` outside any `<…>` surface — the bare
/// counterpart of [`diff_ast_static_const_bare_int`].
#[test]
fn diff_ast_static_const_bare_null() {
    assert_asts_match("(x : null)\n");
}

/// `(x : int * 42)` → `Tuple([int, StaticConstant 42])`. A static constant as a
/// tuple-type segment, confirming the bare `StaticConstant` composes with the
/// `*` tuple layer.
#[test]
fn diff_ast_static_const_in_tuple() {
    assert_asts_match("(x : int * 42)\n");
}

/// `(int).Foo<42>` → `LongIdentApp(Paren(int), [Foo], [StaticConstant 42])`.
/// The static-arg parsing also reaches the `atomType DOT path <…>`
/// (`typeArgsNoHpaDeprecated`) loop, not only the prefix-app `<…>`.
#[test]
fn diff_ast_static_const_in_dot_chain_type_args() {
    assert_asts_match("(x : (int).Foo<42>)\n");
}

/// `float<1>` → `App(float, [StaticConstant(Int32 1)])`. The dimensionless
/// unit-of-measure form: FCS models the lone `1` exactly like any other integer
/// static argument (deferred here from phase 10.8).
#[test]
fn diff_ast_static_const_dimensionless_measure() {
    assert_asts_match("(x : float<1>)\n");
}

/// `float<1/s>` → `App(float, [Tuple([StaticConstant 1, Slash, LongIdent s])])`.
/// The `/` measure-division form: FCS reuses the tuple-type grammar's
/// `SynTupleTypeSegment.Slash` over a leading `StaticConstant` (deferred here
/// from phase 10.8).
#[test]
fn diff_ast_static_const_measure_division() {
    assert_asts_match("(x : float<1/s>)\n");
}

/// `(x : -1)` → `StaticConstant(Int32 -1)`. FCS lexes the sign into the literal
/// token (the `sign_fold` mirror), so a *signed* literal is a valid
/// `rawConstant` static constant. The raw cursor is the pre-fold `Op("-")`, so
/// the type-start gate must consult the filtered (folded) literal.
#[test]
fn diff_ast_static_const_bare_negative() {
    assert_asts_match("(x : -1)\n");
}

/// `(x : +1)` → `StaticConstant(Int32 1)`. The positive-sign fold.
#[test]
fn diff_ast_static_const_bare_positive() {
    assert_asts_match("(x : +1)\n");
}

/// `Foo< -1>` → `App(Foo, [StaticConstant(Int32 -1)])`. A signed static
/// argument in the `<…>` surface (spaced so `<-` doesn't fuse into the
/// back-arrow token).
#[test]
fn diff_ast_static_const_negative_arg() {
    assert_asts_match("(x : Foo< -1>)\n");
}

/// `Foo<N= -1>` → `StaticConstantNamed(N, StaticConstant(Int32 -1))`. A signed
/// value in the named form.
#[test]
fn diff_ast_static_const_named_negative_value() {
    assert_asts_match("(x : Foo<N= -1>)\n");
}

/// `int * -1` → `Tuple([int, StaticConstant -1])`. A signed static constant as
/// a tuple segment, exercising the post-separator gate's acceptance of a folded
/// literal.
#[test]
fn diff_ast_static_const_negative_in_tuple() {
    assert_asts_match("(x : int * -1)\n");
}

/// `float</s>` → `App(float, [Tuple([Slash, LongIdent s])])`. A measure tuple
/// that *opens* with the divisor (reciprocal measure): FCS's leading
/// `INFIX_STAR_DIV_MOD_OP tupleOrQuotTypeElements` arm builds a leading
/// `SynTupleTypeSegment.Slash` with no preceding `Type` segment.
#[test]
fn diff_ast_static_const_leading_slash_measure() {
    assert_asts_match("(x : float</s>)\n");
}

/// `(x : /s)` → `Tuple([Slash, LongIdent s])`. The bare leading-slash measure,
/// outside any `<…>` surface — confirming the leading-`/` `typ` start is
/// general, not gated to the type-arg loop.
#[test]
fn diff_ast_static_const_bare_leading_slash() {
    assert_asts_match("(x : /s)\n");
}

/// `(x : (/s))` → `Paren(Tuple([Slash, LongIdent s]))`. The leading-slash
/// measure inside a paren type: the paren-type inner gate must reach
/// `parse_type` (the `typ`-level start), not the atomic gate that rejects `/`.
#[test]
fn diff_ast_static_const_paren_leading_slash() {
    assert_asts_match("(x : (/s))\n");
}

/// `float<(/s)>` → `App(float, [Paren(Tuple([Slash, s]))])`. A parenthesised
/// leading-slash measure as a type argument.
#[test]
fn diff_ast_static_const_arg_paren_leading_slash() {
    assert_asts_match("(x : float<(/s)>)\n");
}

/// `Foo<const (1)>` reuses the paren-expr operand; pinned with the spaced
/// `Foo<const (1) >`-free form already, but confirm the parenthesised expr is
/// `Paren(Const 1)`.
#[test]
fn diff_ast_static_const_expr_paren_const() {
    assert_asts_match("(x : Foo<const (1)>)\n");
}

/// `type T = /s` → a type abbreviation whose body is the leading-slash measure
/// tuple `Tuple([Slash, LongIdent s])`. Confirms the type-abbreviation body's
/// pre-gate uses the `typ`-level `peek_starts_type` so the leading `/` reaches
/// `parse_type` (not only the typed-paren / paren-inner callers).
#[test]
fn diff_ast_type_abbrev_leading_slash() {
    assert_asts_match("type T = /s\n");
}

/// `#-1` → `HashConstraint(StaticConstant(Int32 -1))`. The `#T` hash-constraint
/// recursion shares the folded-literal-aware `peek_starts_atomic_type` gate, so
/// a sign-folded static constant (raw cursor `Op("-")`, filtered `INT32_LIT`)
/// is accepted as the inner atom rather than left dangling.
#[test]
fn diff_ast_hash_constraint_over_signed_static_const() {
    assert_asts_match("(x : #-1)\n");
}

// ── Phase 10.10 — `SynType.Intersection` (`#A & #B`, `'T & #A`) ──────────────
// FCS's `intersectionType` (`pars.fsy:6328-6335`): a bare `typar` or
// `hashConstraint` head, then a `&`-separated run of flexible-type (`#T`)
// constraints. Reachable from the typed-paren surface `(x : T)` at the
// `appTypeWithoutNull` layer. Ground-truthed via `fcs-dump ast`.

/// `(x : #A & #B)` → `Intersection(None, [Hash A; Hash B])`. The
/// `hashConstraint AMP …` head form: `typar` is `None`, the leading `#A`
/// becomes the first `types` element.
#[test]
fn diff_ast_intersection_two_hash() {
    assert_asts_match("(x : #A & #B)\n");
}

/// `(x : #A & #B & #C)` → a three-operand intersection (flat `types` list).
#[test]
fn diff_ast_intersection_three_hash() {
    assert_asts_match("(x : #A & #B & #C)\n");
}

/// `(x : 'T & #A)` → `Intersection(Some 'T, [Hash A])`. The `typar AMP …` head
/// form: the head typar lands in the dedicated `typar` slot, not in `types`.
#[test]
fn diff_ast_intersection_typar_head() {
    assert_asts_match("(x : 'T & #A)\n");
}

/// `(x : ^T & #A)` → `Intersection(Some ^T, [Hash A])` with the head typar's
/// `staticReq = HeadType`. Pins the `^`-sigil head.
#[test]
fn diff_ast_intersection_head_typar_head() {
    assert_asts_match("(x : ^T & #A)\n");
}

/// `(x : #Foo<int> & #B)` → `Intersection(None, [Hash(App(Foo,[int])); Hash B])`.
/// The head hash constraint carries an inner prefix-app — the `<int>` stays
/// *inside* the `#…`, so the head is still a bare `hashConstraint`.
#[test]
fn diff_ast_intersection_hash_prefix_app_head() {
    assert_asts_match("(x : #Foo<int> & #B)\n");
}

/// `(x : #A & #B -> int)` → `Fun(Intersection([Hash A; Hash B]), int)`. The
/// intersection sits at the `appTypeWithoutNull` layer, below the arrow, so it
/// is the function argument type.
#[test]
fn diff_ast_intersection_arrow_operand() {
    assert_asts_match("(x : #A & #B -> int)\n");
}

/// `(x : #A & #B * int)` → `Tuple([Intersection([Hash A; Hash B]); Star; int])`.
/// The intersection binds tighter than the tuple `*`, so it is the first tuple
/// segment.
#[test]
fn diff_ast_intersection_tuple_segment() {
    assert_asts_match("(x : #A & #B * int)\n");
}

/// `(x : Foo<#A & #B>)` → `App(Foo, [Intersection([Hash A; Hash B])])`. An
/// intersection as a generic type-argument (the `<…>` actual is a full `typ`,
/// which reaches `appTypeWithoutNull`).
#[test]
fn diff_ast_intersection_in_generic_arg() {
    assert_asts_match("(x : Foo<#A & #B>)\n");
}

/// `(x : 'T & 'U)` → `Intersection(Some 'T, [Var 'U])` *with* FCS error 3572
/// (`'U` is not a flexible `#T`). FCS still emits the tree; the harness requires
/// our parser to also report an error.
#[test]
fn diff_ast_intersection_typar_head_non_flexible_tail() {
    assert_asts_match_allow_errors("(x : 'T & 'U)\n");
}

/// `(x : 'T & IDisposable)` → `Intersection(Some 'T, [LongIdent IDisposable])`
/// with FCS error 3572 — a non-`#` tail operand is parsed but flagged.
#[test]
fn diff_ast_intersection_non_flexible_named_tail() {
    assert_asts_match_allow_errors("(x : 'T & IDisposable)\n");
}

/// `(x : #A & #B list)` → `App(list, [Intersection([Hash A; Hash B])])`. FCS
/// applies the `appTypeWithoutNull appTypeConPower` postfix continuation to a
/// reduced `intersectionType`, so the intersection is the *argument* of the
/// `list` postfix-app (the suffix loop must run after the intersection).
#[test]
fn diff_ast_intersection_postfix_app_suffix() {
    assert_asts_match("(x : #A & #B list)\n");
}

/// `(x : #A & #B[])` → `Array(Intersection([Hash A; Hash B]))`. The
/// `appTypeWithoutNull arrayTypeSuffix` continuation wraps the whole reduced
/// intersection in an array type.
#[test]
fn diff_ast_intersection_array_suffix() {
    assert_asts_match("(x : #A & #B[])\n");
}

/// `(x : Foo<^T & #A>)` → `App(Foo, [Intersection(Some ^T, [Hash A])])`. The
/// SRTP typar head `^T` as the *first* generic argument fuses with the `<`
/// opener into a single raw `<^`, which LexFilter splits into `<` + `^`. After
/// the `<` is bumped, the head lookahead must see the pending `^` split tail
/// (not the `T` ident behind it) to open the intersection.
#[test]
fn diff_ast_intersection_head_typar_in_generic_arg() {
    assert_asts_match("(x : Foo<^T & #A>)\n");
}

// ---- measure-power heads reject a following type-arg block --------------
//
// FCS's `appTypeCon typeArgsNoHpaDeprecated` (`pars.fsy:6596`) wraps a
// *plain* `appTypeCon` in `<…>` type args, but not an `appTypeConPower`
// (`appTypeCon ^ exp`). So after a measure-power head (`Foo^2`), a
// following `<…>` — adjacent or spaced — is rejected: FCS parses `Foo^2`
// as a `MeasurePower` type and reports "unexpected type application" /
// "unexpected `<`". Our parser likewise declines the type-arg wrap and
// surfaces the leftover marker / `<` as the enclosing context's error.
// These are pinned for error + losslessness rather than diffed: FCS's
// invalid-input recovery tree contains nodes our normaliser does not
// model, so a full AST diff is not meaningful here.

/// Adjacent type args after a measure-power head — `Foo^2<int>`. Pins that
/// the head is *not* upgraded to an `APP_TYPE` over the power (the
/// pre-existing silent-accept bug the spaced-generic work exposed).
#[test]
fn measure_power_head_rejects_adjacent_type_args() {
    let p = parse("let x : Foo^2<int> = y\n");
    assert!(
        !p.errors.is_empty(),
        "type args after a measure-power head must be rejected (FCS errors too)",
    );
    assert_eq!(p.root.text().to_string(), "let x : Foo^2<int> = y\n");
}

/// Spaced type args after a measure-power head — `Foo^2 < int >`. The
/// spaced-generic `appTypeCon` wrap must inherit the same `appTypeConPower`
/// exclusion as the adjacent form.
#[test]
fn measure_power_head_rejects_spaced_type_args() {
    let p = parse("let x : Foo^2 < int > = y\n");
    assert!(
        !p.errors.is_empty(),
        "spaced type args after a measure-power head must be rejected (FCS errors too)",
    );
    assert_eq!(p.root.text().to_string(), "let x : Foo^2 < int > = y\n");
}

// --- `global`-rooted type paths -------------------------------------------
//
// FCS admits the `global` keyword as the root of a qualified type path
// (`global.System.Int32`, `global.string`), spelling the head segment as the
// single-backtick-quoted identifier `` `global` `` (the keyword reused as an
// identifier) exactly as in expression position. The whole path is a
// `SynType.LongIdent(["global"; …])`. The normaliser strips a single
// surrounding backtick pair per segment, so both sides line up on `global`.

/// A `global`-rooted qualified type `global.System.Int32` →
/// `LongIdent(["global"; "System"; "Int32"])`.
#[test]
fn diff_global_qualified_type() {
    assert_asts_match("let i : global.System.Int32 = 0\n");
}

/// A short `global`-rooted type `global.string` in a parameter annotation →
/// `LongIdent(["global"; "string"])`.
#[test]
fn diff_global_type_short_path() {
    assert_asts_match("let f (x : global.string) = ()\n");
}

/// A single-qualification `global`-rooted type `global.Foo` →
/// `LongIdent(["global"; "Foo"])`.
#[test]
fn diff_global_type_single_qualification() {
    assert_asts_match("let x : global.Foo = y\n");
}

/// A `global`-rooted type as the constructor of a postfix type application
/// (`int global.Foo` ⇒ `global.Foo<int>`). FCS admits `global` as an
/// `appTypeCon` postfix head, so it must be in `raw_starts_postfix_app_head`.
#[test]
fn diff_global_postfix_type_app() {
    assert_asts_match("let x : int global.Foo = y\n");
}
