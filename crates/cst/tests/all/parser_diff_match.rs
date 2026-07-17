//! Differential test (`parser::parse` vs FCS): `match` expressions and the
//! pattern surface (list-cons, `&`-and, `|`-or patterns). Split out of the
//! former monolithic `parser_diff.rs`.

use crate::common::{assert_asts_match, assert_asts_match_allow_errors};

/// Phase 6.5 — record pattern `{ X = a }` in a match clause. Atomic-level
/// (`pars.fsy:3780` `LBRACE recordPatternElementsAux rbrace`), projecting to
/// `SynPat.Record([NamePatPairField(["X"], =, Named a)], _)`. The `{` is
/// emitted but the closing `}` is LexFilter-swallowed (like `)`), so it must
/// be reclaimed from the raw stream.
#[test]
fn diff_ast_match_record_pat() {
    assert_asts_match("match r with { X = a } -> 1\n");
}

/// Phase 6.5 — two fields separated by `;` (the record-field separator, like
/// list patterns — `,` would be a tuple *inside* one field's value).
#[test]
fn diff_ast_match_record_pat_two_fields() {
    assert_asts_match("match r with { X = a; Y = b } -> 1\n");
}

/// Phase 6.5 — a qualified field name `M.X` (the field name is FCS's `path`,
/// a `SynLongIdent`, not a bare ident).
#[test]
fn diff_ast_match_record_pat_qualified() {
    assert_asts_match("match r with { M.X = a } -> 1\n");
}

/// Phase 6.5 — a record pattern nested as a field value: `{ X = { Y = a } }`.
/// The field value is a full `parenPattern`, so it recurses into another
/// atomic record pattern.
#[test]
fn diff_ast_match_record_pat_nested() {
    assert_asts_match("match r with { X = { Y = a } } -> a\n");
}

/// Phase 6.5 — a nested record field value *followed by another field*:
/// `{ X = { Y = a }; Z = b }`. The inner record's closing `}` is swallowed
/// from the filtered stream, so a naive field loop would see the outer `;`
/// (which separates `X` and `Z`) via `peek()` and consume it as an *inner*
/// field separator, draining the real inner `}` as an error. The inner field
/// loop must check for the swallowed `}` (raw stream) before looking at a
/// filtered separator.
#[test]
fn diff_ast_match_record_pat_nested_then_field() {
    assert_asts_match("match r with { X = { Y = a }; Z = b } -> 1\n");
}

/// Phase 6.5 — a tuple as a field value: `{ X = a, b }` is *one* field whose
/// value is `Tuple[a, b]` (the `,` is folded into the value by
/// `wrap_pat_tail`, exactly as in list patterns). Pins that the field-value
/// parse runs the full in-delimiter pattern, not just an atom.
#[test]
fn diff_ast_match_record_pat_tuple_value() {
    assert_asts_match("match r with { X = a, b } -> 1\n");
}

/// Phase 6.5 — an `as` tail on a record pattern: `{ X = a } as z`. Confirms
/// the atomic record pattern composes with the existing `wrap_pat_tail`
/// ladder (`As(Record, z)`).
#[test]
fn diff_ast_match_record_pat_as() {
    assert_asts_match("match r with { X = a } as z -> 1\n");
}

/// Phase 6.6 — the dynamic type-test pattern `:? T` (`SynPat.IsInst`). FCS's
/// `constrPattern: COLON_QMARK atomTypeOrAnonRecdType` (`pars.fsy:3729`), one
/// level above the atomic patterns, so it parses through the shared
/// head-binding entry. `int` is a single-segment `SynType.LongIdent`.
#[test]
fn diff_ast_match_isinst_pat() {
    assert_asts_match("match x with :? int -> 1\n");
}

/// Phase 6.6 — a dotted type after `:?`: `:? System.Exception`. The type is a
/// multi-segment `SynType.LongIdent`, exercising the long-ident type parser
/// inside the IsInst pattern.
#[test]
fn diff_ast_match_isinst_pat_dotted() {
    assert_asts_match("match x with :? System.Exception -> 1\n");
}

/// Phase 6.6 — a generic application type after `:?`: `:? List<int>`. The
/// `atomTypeOrAnonRecdType` level admits the prefix-app form (FCS's
/// `appTypeConPower`), so the type projects to `SynType.App(List, [int],
/// isPostfix=false)`. Pins that the IsInst type parse reaches the
/// HighPrecedenceTyApp wrap.
#[test]
fn diff_ast_match_isinst_pat_generic() {
    assert_asts_match("match x with :? List<int> -> 1\n");
}

/// Phase 6.6 — an `as` tail on an IsInst pattern: `:? exn as e`. The IsInst is
/// the `as`-left operand, so it composes with the existing `wrap_pat_tail`
/// ladder (`As(IsInst(exn), Named e)`). The classic downcast-and-bind form.
#[test]
fn diff_ast_match_isinst_pat_as() {
    assert_asts_match("match x with :? exn as e -> e\n");
}

/// Phase 6.6 — an IsInst pattern as a *list* element: `[ :? int ]`
/// (`ArrayOrList([IsInst(int)])`). FCS accepts the same-line form (a list
/// pattern whose sole element is the type-test). Pins that the IsInst hook in
/// the shared head-binding entry reaches the list-element path
/// (`emit_paren_pat_element`). The offside form (`[ :?⏎ int ]`, where a
/// `Virtual::BlockSep` parks between `:?` and the type) is a recovery case —
/// FCS errors and our parser must not panic — covered by the
/// `isinst_pat_list_element_offside_recovers` unit test.
#[test]
fn diff_ast_match_isinst_list_element() {
    assert_asts_match("match x with [ :? int ] -> 1\n");
}

// ---- Phase 6.7 — ListCons patterns (`h :: t`) + precedence-climbing tail ----
//
// `::` (`SynPat.ListCons`, `pars.fsy:3944`, `%right COLON_COLON` at `:361`) is
// the *tightest* infix pattern operator and right-associative. Adding it
// converts the pattern tail (`wrap_pat_tail`) from a token-order re-wrap loop
// into a precedence climber over FCS's yacc ladder. The cross-precedence cases
// below were all ground-truthed against `fcs-dump`.

/// Phase 6.7 — the minimal cons pattern `h :: t` ⇒ `ListCons(h, t)`.
#[test]
fn diff_ast_match_cons_pat() {
    assert_asts_match("match v with h :: t -> 1\n");
}

/// Phase 6.7 — right-associativity: `a :: b :: c` ⇒
/// `ListCons(a, ListCons(b, c))` (right-nested), forced by `%right
/// COLON_COLON`. The climber parses the `::` rhs at its *own* binding power so
/// a following `::` nests into the rhs.
#[test]
fn diff_ast_match_cons_right_assoc() {
    assert_asts_match("match v with a :: b :: c -> 1\n");
}

/// Phase 6.7 — `::` binds tighter than `,`: `a :: b, c` ⇒
/// `Tuple[ListCons(a, b), c]` (the cons groups inside the first tuple
/// element, not the tuple inside the cons).
#[test]
fn diff_ast_match_cons_tighter_than_comma() {
    assert_asts_match("match v with a :: b, c -> 1\n");
}

/// Phase 6.7 — the mirror: `a, b :: c` ⇒ `Tuple[a, ListCons(b, c)]`. The
/// second tuple element absorbs the `:: c` (the comma element climbs at a
/// binding power that captures `::` but stops at the next `,`).
#[test]
fn diff_ast_match_comma_then_cons() {
    assert_asts_match("match v with a, b :: c -> 1\n");
}

/// Phase 6.7 — a const head: `1 :: rest` ⇒ `ListCons(Const 1, rest)`.
#[test]
fn diff_ast_match_cons_const_head() {
    assert_asts_match("match v with 1 :: rest -> 1\n");
}

/// Phase 6.7 — a constructor-application head: `Some x :: rest` ⇒
/// `ListCons(LongIdent("Some", [x]), rest)`. The cons operands are full
/// applPats, not just atoms.
#[test]
fn diff_ast_match_cons_ctor_head() {
    assert_asts_match("match v with Some x :: rest -> 1\n");
}

/// Phase 6.7 — `as` (loosest) wraps a cons: `a :: b as c` ⇒
/// `As(ListCons(a, b), c)`.
#[test]
fn diff_ast_match_cons_then_as() {
    assert_asts_match("match v with a :: b as c -> 1\n");
}

/// Phase 6.7 — the structural-grammar subtlety `a as b :: c` ⇒
/// `ListCons(As(a, b), c)`. The `as` rhs is `constrPattern` (it cannot
/// contain `::`), so the `as` reduces first and the `::` then wraps the whole
/// `As` — i.e. `::` ends up *outer* here, unlike `a :: b as c`. Pins that the
/// climber's atom-only `as`-rhs reproduces FCS's resolution.
#[test]
fn diff_ast_match_as_then_cons() {
    assert_asts_match("match v with a as b :: c -> 1\n");
}

/// Phase 6.7 — three-way precedence: `a, b, c :: d` ⇒
/// `Tuple[a, b, ListCons(c, d)]` (flat n-ary tuple, last element a cons).
#[test]
fn diff_ast_match_tuple_run_with_cons() {
    assert_asts_match("match v with a, b, c :: d -> 1\n");
}

// ---- Phase 6.8 — Ands patterns (`a & b & c`) ----
//
// `&` (`SynPat.Ands`, `pars.fsy:3649`/`:4000`, `%left AMP` at `:355`) is the
// n-ary conjunction operator — tighter than `,`/`:`/`as`, looser than `::`. It
// adds the `AMP` rung to the precedence climber. All cross-precedence shapes
// below were ground-truthed against `fcs-dump`.

/// Phase 6.8 — the minimal conjunction `a & b` ⇒ `Ands[a, b]`.
#[test]
fn diff_ast_match_ands_pat() {
    assert_asts_match("match v with a & b -> 1\n");
}

/// Phase 6.8 — n-ary flat list: `a & b & c` ⇒ `Ands[a, b, c]` (one flat
/// `Ands`, not nested), the n-ary gather at the `AMP` rung.
#[test]
fn diff_ast_match_ands_three() {
    assert_asts_match("match v with a & b & c -> 1\n");
}

/// Phase 6.8 — `::` binds tighter than `&`: `a & b :: c` ⇒
/// `Ands[a, ListCons(b, c)]` (the cons groups inside the second operand).
#[test]
fn diff_ast_match_ands_cons_tighter() {
    assert_asts_match("match v with a & b :: c -> 1\n");
}

/// Phase 6.8 — the mirror: `a :: b & c` ⇒ `Ands[ListCons(a, b), c]`.
#[test]
fn diff_ast_match_cons_then_ands() {
    assert_asts_match("match v with a :: b & c -> 1\n");
}

/// Phase 6.8 — `&` binds tighter than `,`: `a & b, c` ⇒
/// `Tuple[Ands[a, b], c]`.
#[test]
fn diff_ast_match_ands_tighter_than_comma() {
    assert_asts_match("match v with a & b, c -> 1\n");
}

/// Phase 6.8 — the mirror: `a, b & c` ⇒ `Tuple[a, Ands[b, c]]`.
#[test]
fn diff_ast_match_comma_then_ands() {
    assert_asts_match("match v with a, b & c -> 1\n");
}

/// Phase 6.8 — `as` (loosest) wraps a conjunction: `a & b as c` ⇒
/// `As(Ands[a, b], c)`.
#[test]
fn diff_ast_match_ands_then_as() {
    assert_asts_match("match v with a & b as c -> 1\n");
}

/// Phase 6.8 — the structural-grammar subtlety `a as b & c` ⇒
/// `Ands[As(a, b), c]`: the `as` rhs is `constrPattern` (it can't contain
/// `&`), so the `as` reduces first and the `&` then gathers it — `&` ends up
/// *outer*, unlike `a & b as c`. Pins that the climber's atom-only `as`-rhs
/// reproduces FCS's resolution.
#[test]
fn diff_ast_match_as_then_ands() {
    assert_asts_match("match v with a as b & c -> 1\n");
}

// ---- Phase 6.9 — Or patterns (`A | B`) ----
//
// `|` (`SynPat.Or`, `pars.fsy:3584`/`:3916`, `%left BAR` at `:266`) is the
// *loosest* infix pattern operator, left-associative — the final rung of the
// precedence climber. It also composes with the `match`-clause loop: a `|`
// *before* `->` is an or-separator (consumed by the climber); a `|` *after*
// `-> result` is the clause separator (consumed by `parse_match_clauses`,
// which stays untouched). All shapes ground-truthed against `fcs-dump`.

/// Phase 6.9 — the minimal or-pattern `A | B` ⇒ `Or(A, B)` (one clause).
#[test]
fn diff_ast_match_or_pat() {
    assert_asts_match("match v with A | B -> 1\n");
}

/// Phase 6.9 — left-associativity: `A | B | C` ⇒ `Or(Or(A, B), C)`
/// (left-nested), the outer climber loop re-wrapping `cp`.
#[test]
fn diff_ast_match_or_left_assoc() {
    assert_asts_match("match v with A | B | C -> 1\n");
}

/// Phase 6.9 — `,` binds tighter than `|`: `A | B, C` ⇒
/// `Or(A, Tuple[B, C])` (the comma groups inside the or's rhs).
#[test]
fn diff_ast_match_or_comma_in_rhs() {
    assert_asts_match("match v with A | B, C -> 1\n");
}

/// Phase 6.9 — the mirror: `A, B | C` ⇒ `Or(Tuple[A, B], C)` (the `|`
/// wraps the whole tuple).
#[test]
fn diff_ast_match_comma_then_or() {
    assert_asts_match("match v with A, B | C -> 1\n");
}

/// Phase 6.9 — `as` (still looser than `|`) wraps an or: `a | b as c` ⇒
/// `As(Or(a, b), c)`.
#[test]
fn diff_ast_match_or_then_as() {
    assert_asts_match("match v with a | b as c -> 1\n");
}

/// Phase 6.9 — `::` (tighter) binds inside the or's lhs: `a :: b | c` ⇒
/// `Or(ListCons(a, b), c)`.
#[test]
fn diff_ast_match_cons_then_or() {
    assert_asts_match("match v with a :: b | c -> 1\n");
}

/// Phase 6.9 — the `| null` form: `A | null` ⇒ `Or(A, Null)`. Our lexfilter
/// doesn't relabel `BAR_JUST_BEFORE_NULL` (the type-level `T | null` is handled
/// separately in 7.11), so the `|` is a plain `Token::Bar` and the rhs is just
/// a null-pat.
#[test]
fn diff_ast_match_or_null() {
    assert_asts_match("match v with A | null -> 1\n");
}

/// Phase 6.9 — disambiguation: `A | B -> 1` is **one** clause with an
/// or-pattern (above), but `A -> 1 | B -> 2` is **two** clauses — the `|`
/// after `-> 1` is the clause separator, not an or-separator. The climber
/// stops at `->`, so `parse_match_clauses` (unchanged) owns this `|`. Regression
/// guard that the or-pattern parse doesn't disturb the multi-clause loop.
#[test]
fn diff_ast_match_two_clauses_not_or() {
    assert_asts_match("match v with A -> 1 | B -> 2\n");
}

/// Phase 6.9 — both at once: `A | B -> 1 | C -> 2` ⇒ two clauses, clause 1's
/// pattern is `Or(A, B)`. The first `|` (in pattern position) is an
/// or-separator; the second (after `-> 1`) is the clause separator.
#[test]
fn diff_ast_match_or_then_clause_sep() {
    assert_asts_match("match v with A | B -> 1 | C -> 2\n");
}

/// Phase 6.9 — a leading bar followed by an or-pattern: `| A | B -> 1` ⇒ one
/// clause `Or(A, B)`. The leading `|` is consumed by `parse_match_clauses`
/// before the pattern entry, so only the *inner* `|` is the or-separator.
#[test]
fn diff_ast_match_leading_bar_with_or() {
    assert_asts_match("match v with | A | B -> 1\n");
}

/// Phase 5.M.1 — `match x with A -> 1`: the smallest `match` expression.
/// One clause, no leading `|`, no `when`. `A` is uppercase so the clause
/// pattern projects to `SynPat.LongIdent(["A"], …)` (phase-5.X.1 promotion),
/// the scrutinee is a plain `Ident`, and the result is a `Const`. Pins the
/// `MATCH_EXPR > [MATCH_TOK, scrut, WITH_TOK, MATCH_CLAUSE]` shape against
/// FCS's `SynExpr.Match(_, Ident x, [Clause(LongIdent A, None, 1)], …)`.
#[test]
fn diff_ast_match_single_clause() {
    assert_asts_match("match x with A -> 1\n");
}

/// Phase 5.M.1 — `match x with Some y -> y`: a constructor-application
/// clause pattern. The clause-pattern entry reuses the head-binding
/// function-form sweep, so `Some y` projects to
/// `SynPat.LongIdent(["Some"], [Named y])` exactly as FCS's `parenPattern`
/// does. Result is the bound `y`.
#[test]
fn diff_ast_match_ctor_clause() {
    assert_asts_match("match x with Some y -> y\n");
}

/// An *adjacent* parenthesised constructor argument `Some(x)` in a clause
/// pattern. The clause entry reuses the head-binding sweep, so the
/// `HighPrecedenceParenApp` virtual before the `(` is skipped and the arg
/// parses to `SynPat.LongIdent(["Some"], [Paren(Named "x")])` — matching the
/// spaced `Some (x)`.
#[test]
fn diff_ast_match_ctor_clause_adjacent_paren() {
    assert_asts_match("match x with Some(x) -> x | _ -> 0\n");
}

/// Phase 5.M.1 — `match (a, b) with x, y -> x`: a comma-separated tuple
/// clause pattern (no parens — `parenPattern` allows a top-level tuple).
/// `wrap_pat_tail`'s comma arm produces `SynPat.Tuple([Named x, Named y])`.
/// Also exercises a parenthesised tuple *scrutinee*.
#[test]
fn diff_ast_match_tuple_clause() {
    assert_asts_match("match (a, b) with x, y -> x\n");
}

/// Phase 5.M.1 — `let f x = match x with A -> 1`: the `match` is the RHS of
/// a function-form `let`. Pins that `parse_match_expr` drains exactly its
/// own trailing `RightBlockEnd` + `End`, leaving the enclosing let's
/// `BlockEnd`/`DeclEnd` for `parse_let_binding` to consume — the same
/// careful single-pair drain `parse_fun_expr` uses.
#[test]
fn diff_ast_match_as_let_rhs() {
    assert_asts_match("let f x = match x with A -> 1\n");
}

/// Phase 5.M.2 — `match x with A -> 1 | B -> 2`: two clauses on one line,
/// separated by a bare `|` (no leading bar). The single-line LexFilter shape
/// puts the `|` between clauses as a raw `Bar` with no surrounding block-end,
/// and emits exactly one trailing `RightBlockEnd`+`End`. Each clause owns its
/// own `BAR_TOK` to mirror FCS's per-clause `BarRange` (the first clause has
/// none).
#[test]
fn diff_ast_match_two_clauses() {
    assert_asts_match("match x with A -> 1 | B -> 2\n");
}

/// Phase 5.M.2 — `match x with | A -> 1 | B -> 2`: an optional *leading* bar
/// before the first clause. FCS elides the leading-bar range, so both forms
/// (with and without the leading `|`) must project identically.
#[test]
fn diff_ast_match_leading_bar() {
    assert_asts_match("match x with | A -> 1 | B -> 2\n");
}

/// Phase 5.M.2 — three single-line clauses, exercising the loop past two
/// iterations.
#[test]
fn diff_ast_match_three_clauses() {
    assert_asts_match("match x with A -> 1 | B -> 2 | C -> 3\n");
}

/// Phase 5.M.2 — offside multi-line clauses. Each clause sits on its own line
/// under a `let`, so the LexFilter closes each clause with its own
/// `RightBlockEnd` *before* the next `Bar`, then a single final `End`. The
/// parser's per-clause optional-`RightBlockEnd` drain plus `peek()==Bar`
/// continuation must handle this distinct token shape.
#[test]
fn diff_ast_match_offside_clauses() {
    assert_asts_match("let f x =\n    match x with\n    | A -> 1\n    | B -> 2\n");
}

/// Phase 5.M.2 — a constructor-application clause pattern in a multi-clause
/// context, confirming the head-binding sweep still fires per clause.
#[test]
fn diff_ast_match_ctor_in_multi() {
    assert_asts_match("match x with Some y -> y | None -> 0\n");
}

/// Phase 5.M.2 — each clause result is itself a `fun` lambda, offside on its
/// own line. This nests the single-pair virtual drains: `parse_fun_expr`
/// consumes the lambda's own `RightBlockEnd`+`End`, leaving the clause's
/// SeqBlock `RightBlockEnd` for `parse_match_expr`'s per-clause drain, then a
/// `Bar` for the next clause and finally the lone `CtxtMatchClauses` `End`.
/// Guards against either drain stealing the other's close virtuals.
#[test]
fn diff_ast_match_fun_lambda_clause() {
    assert_asts_match("match x with\n| A -> fun y -> y\n| B -> fun z -> z\n");
}

/// Phase 5.M.3 — `match x with A when y -> 1`: the minimal `when`-guard form.
/// `when` is a bare raw token between the clause pattern and `->`; the guard
/// is an ordinary expression. FCS projects the guard into `SynMatchClause`'s
/// `whenExpr: SynExpr option` (here `Some(Ident y)`).
#[test]
fn diff_ast_match_when_guard() {
    assert_asts_match("match x with A when y -> 1\n");
}

/// Phase 5.M.3 — a guard on one clause among several. The second clause has
/// no guard (`whenExpr = None`), confirming the optional-`when` parse fires
/// per clause.
#[test]
fn diff_ast_match_when_among_clauses() {
    assert_asts_match("match x with A when y -> 1 | B -> 2\n");
}

/// Phase 5.M.3 — a relational guard expression with a wildcard fall-through
/// clause: `match n with n when n > 0 -> 1 | _ -> 0`. Confirms the guard
/// `parse_expr` consumes a full infix expression and stops at `->`.
#[test]
fn diff_ast_match_when_relational() {
    assert_asts_match("match n with n when n > 0 -> 1 | _ -> 0\n");
}

/// Phase 5.M.3 — offside multi-line guarded clauses under a `let`. Pins that
/// the `when`-guard parse composes with the offside SeqBlock-close handling.
#[test]
fn diff_ast_match_when_offside() {
    assert_asts_match("let f x =\n    match x with\n    | A when g -> 1\n    | B -> 2\n");
}

/// Phase 5.M.3 — a `fun` lambda in *both* the guard and the result. FCS's
/// `SimplePatsOfPat` lowering assigns the shared `_argN` placeholder in
/// source order, so the guard's lambda must be `_arg1` and the result's
/// `_arg2`. The normaliser must therefore project the guard before the
/// result — this test pins that ordering.
#[test]
fn diff_ast_match_when_fun_lambda_ordering() {
    assert_asts_match("match x with A when (fun a -> a) -> (fun b -> b)\n");
}

/// Phase 5.M.5 — an offside multi-statement clause *body*:
/// ```fsharp
/// match x with
/// | A ->
///     e1
///     e2
/// ```
/// The `->` opens a one-sided SeqBlock; the body's statements are separated
/// by `Virtual::BlockSep` and closed by the clause's `RightBlockEnd`. FCS
/// projects the result to `SynExpr.Sequential(SuppressNeither, e1, e2)`, so
/// the clause result must be wrapped in `SEQUENTIAL_EXPR`, mirroring
/// `parse_fun_expr`'s body handling. The `BlockSep` loop must stop at the
/// per-clause `RightBlockEnd` so the clause-list close survives.
#[test]
fn diff_ast_match_seq_body() {
    assert_asts_match("match x with\n| A ->\n    e1\n    e2\n");
}

/// Phase 5.M.5 — three statements in a clause body. FCS nests
/// `Sequential` right-leaningly (`Sequential(e1, Sequential(e2, e3))`); the
/// normaliser flattens both that and our n-ary `SEQUENTIAL_EXPR` to a
/// three-element list, so two `BlockSep`-separated statements past the first
/// must all land under one wrapper.
#[test]
fn diff_ast_match_seq_body_three_statements() {
    assert_asts_match("match x with\n| A ->\n    e1\n    e2\n    e3\n");
}

/// Phase 5.M.5 — a `when` guard followed by a sequential body. The guard is
/// the *leading* `Expr` child (gated on `WHEN_TOK`); the sequential body is
/// the *trailing* `Expr` child (the `SEQUENTIAL_EXPR`). Pins that wrapping
/// the body does not disturb the positional guard/result disambiguation.
#[test]
fn diff_ast_match_seq_body_with_guard() {
    assert_asts_match("match x with\n| A when c ->\n    e1\n    e2\n");
}

/// Phase 5.M.5 — a sequential body on the *first* of several clauses. Clause
/// A's body `BlockSep`s between `e1`/`e2` and is closed by its own
/// `RightBlockEnd` before the `Bar`; clause B is an ordinary single-expr
/// clause. Confirms the `BlockSep` loop is per-clause and stops at the
/// clause boundary rather than folding clause B into clause A.
#[test]
fn diff_ast_match_seq_body_in_multi_clause() {
    assert_asts_match("match x with\n| A ->\n    e1\n    e2\n| B -> e3\n");
}

/// Phase 5.M.5 — regression guard: a sequential clause body followed by a
/// *sibling* top-level decl. After the body the stream is `RightBlockEnd`
/// (clause close), `End` (clause-list close), then a top-level `BlockSep`
/// and `y`. The clause's `BlockSep` loop must stop at the `RightBlockEnd`,
/// or it would swallow the top-level separator and fold `y` into the match
/// body — dropping the decl count from two to one (the `match`-clause analog
/// of `diff_ast_fun_lambda_nested_lambda_body_with_sibling`).
#[test]
fn diff_ast_match_seq_body_then_sibling_decl() {
    assert_asts_match("match x with\n| A ->\n    e1\n    e2\ny\n");
}

/// Phase 5.M.5 — a clause whose body is itself a `fun` lambda with its own
/// multi-statement body. The inner `BlockSep` belongs to the lambda body and
/// is consumed by `parse_fun_expr`, which also drains the *inner*
/// `RightBlockEnd`+`End` pair; the clause level therefore sees a single
/// statement (the lambda) and the outer `RightBlockEnd`+`End` close the
/// clause list. Pins that the nested single-pair drains compose without the
/// clause's `BlockSep` loop mistaking the lambda's separator for its own.
#[test]
fn diff_ast_match_clause_nested_fun_seq_body() {
    assert_asts_match("match x with\n| A ->\n    fun y ->\n        e1\n        e2\n");
}

/// Stage 2 — explicit `;` sequential in a match-clause result:
/// `match z with _ -> a; b`. FCS's `typedSequentialExprBlockR` accepts a
/// raw `Token::Semi` separator, so the clause result is
/// `SynExpr.Sequential(Ident a, Ident b)`.
#[test]
fn diff_ast_match_semi_seq_body() {
    assert_asts_match("match z with _ -> a; b\n");
}

// ============================================================================
// Phase 10.6 — attributed patterns at a clause head (`SynPat.Attrib`)
// ============================================================================
//
// A `match`/`function` clause head is a full `parenPattern`, so a leading
// `[< … >]` is a valid attributed pattern there just as inside parens
// (ground-truthed against `fcs-dump`). `parse_match_clauses` routes the head
// through the same `emit_attrib_pat` hook with `in_paren = false`.

/// Attributed `match` clause head `match v with [<A>] x -> x` →
/// `clause(Attrib(Named x, [[A]]) -> x)`.
#[test]
fn diff_ast_match_clause_attrib_head() {
    assert_asts_match("match v with [<A>] x -> x\n");
}

/// Non-simple inner at a clause head: `match v with [<A>] Some x -> x` →
/// `clause(Attrib(LongIdent Some [x]) -> x)`. The clause pattern is not
/// lowered (no `_argN`), so the `Attrib` stays in the clause pattern.
#[test]
fn diff_ast_match_clause_attrib_non_simple() {
    assert_asts_match("match v with [<A>] Some x -> x\n");
}

/// Precedence at a clause head — `|` binds *outside* the attrib, so
/// `match v with [<A>] A | B -> A` is `clause(Or(Attrib(A), B) -> A)`, with the
/// or-pattern (not the clause separator) wrapping the `ATTRIB_PAT`.
#[test]
fn diff_ast_match_clause_attrib_or_outside() {
    assert_asts_match("match v with [<A>] A | B -> A\n");
}

/// `as` binds *outside* the attrib at a clause head too:
/// `match v with [<A>] x as y -> x` → `clause(As(Attrib(x), y) -> x)`.
#[test]
fn diff_ast_match_clause_attrib_as_outside() {
    assert_asts_match("match v with [<A>] x as y -> x\n");
}

/// `function` clauses share `parse_match_clauses`, so an attributed head works
/// there as well: `function [<A>] x -> x` → MatchLambda with one
/// `Attrib`-headed clause.
#[test]
fn diff_ast_function_clause_attrib_head() {
    assert_asts_match("function [<A>] x -> x\n");
}

/// Attributed operand in a clause *tail* (after `|`): `match v with A | [<B>] x
/// -> x` → `clause(Or(A, Attrib(x)) -> x)`. Every clause operand is a
/// `parenPattern`, so the `|` rhs admits the attribute prefix — pins that the
/// tail (not just the head) routes through `emit_attrib_pat`.
#[test]
fn diff_ast_match_clause_attrib_tail_or() {
    assert_asts_match("match v with A | [<B>] x -> x\n");
}

/// Attributed operand in a tuple clause tail: `match v with x, [<B>] y -> y` →
/// `clause(Tuple[x, Attrib(y)] -> y)`.
#[test]
fn diff_ast_match_clause_attrib_tail_tuple() {
    assert_asts_match("match v with x, [<B>] y -> y\n");
}

/// Dotted DU clause pattern — `match foo with Foo.Bar -> ()`: a *nullary*
/// multi-segment long-ident pattern (FCS's `atomicPatternLongIdent: pathOp`).
/// The whole `Foo.Bar` path becomes one `SynPat.LongIdent` with a two-segment
/// `SynLongIdent`; our `LONG_IDENT_PAT > LONG_IDENT[Foo . Bar]` projects to the
/// same head segments.
#[test]
fn diff_ast_match_dotted_nullary_du() {
    assert_asts_match("match foo with Foo.Bar -> ()\n");
}

/// Dotted DU clause pattern with an argument — `match foo with Foo.Bar x -> ()`:
/// `atomicPatternLongIdent atomicPatsOrNamePatPairs` → `SynPat.LongIdent` over
/// the two-segment path with one `Named` arg.
#[test]
fn diff_ast_match_dotted_applied_du() {
    assert_asts_match("match foo with Foo.Bar x -> ()\n");
}

/// Three-segment dotted DU clause pattern — `match foo with A.B.C -> ()`:
/// every `.seg` in the dot-continuation folds into the head `SynLongIdent`.
#[test]
fn diff_ast_match_dotted_three_segment_du() {
    assert_asts_match("match foo with A.B.C -> ()\n");
}

/// Lowercase-headed multi-segment clause pattern — `match foo with a.B -> ()`:
/// still `SynPat.LongIdent` (FCS promotes *any* multi-segment path, not just
/// uppercase heads — `pars.fsy:3810`, `not (isNilOrSingleton …)`), never a
/// `SynPat.Named`.
#[test]
fn diff_ast_match_dotted_lowercase_head_du() {
    assert_asts_match("match foo with a.B -> ()\n");
}

/// Dotted DU pattern as a `::`-cons operand — `match foo with Foo.Bar :: t ->
/// ()`: the multi-segment head composes with the pattern infix tail, so the
/// `LONG_IDENT_PAT` lands as the cons head, not just the bare `Foo`.
#[test]
fn diff_ast_match_dotted_du_cons() {
    assert_asts_match("match foo with Foo.Bar :: t -> ()\n");
}

/// Named-field union-case pattern — `match x with Case (fieldName = value) -> 1`.
/// FCS's `atomicPatsOrNamePatPairs → LPAREN namePatPairs rparen`
/// (`pars.fsy:3750`), projecting to `SynPat.LongIdent(["Case"], …,
/// SynArgPats.NamePatPairs([NamePatPairField(["fieldName"], =, Named value)]))`.
/// The single named field's value is a full `parenPattern`. The closing `)` is
/// LexFilter-swallowed, so it must be reclaimed from the raw stream.
#[test]
fn diff_ast_match_name_pat_pairs_single() {
    assert_asts_match("match x with Case (fieldName = value) -> 1\n");
}

/// Two named fields separated by `;` (FCS's `namePatPairs` uses `seps_block`,
/// the same separator record patterns use — a `,` would be an FCS parse error,
/// not a field separator).
#[test]
fn diff_ast_match_name_pat_pairs_two_fields() {
    assert_asts_match("match x with Case (a = 1; b = 2) -> 1\n");
}

/// The named-field group adjacent to the case name (`Case(a = 1)`, no space) —
/// LexFilter inserts a `HighPrecedenceParenApp` virtual before the `(`, which
/// the detector skips and the group emit consumes, so it projects identically
/// to the spaced form.
#[test]
fn diff_ast_match_name_pat_pairs_adjacent() {
    assert_asts_match("match x with Case(a = 1) -> 1\n");
}

/// A named field whose value is itself an applied union-case pattern
/// (`a = Some y`) — the value is a full `parenPattern`, so it recurses through
/// the `constrPattern` level.
#[test]
fn diff_ast_match_name_pat_pairs_applied_value() {
    assert_asts_match("match x with Case (a = Some y) -> 1\n");
}

/// A named field whose value is a tuple pattern (`a = (b, c)`) — the tuple is
/// parenthesised *inside* the field value, distinct from the `;`-separated
/// field list.
#[test]
fn diff_ast_match_name_pat_pairs_tuple_value() {
    assert_asts_match("match x with Case (a = (b, c)) -> 1\n");
}

/// A dotted union-case head with named fields (`Foo.Bar (x = y)`) — FCS's
/// `atomicPatternLongIdent` head is a multi-segment path, so the named group
/// attaches to the whole `SynLongIdent`.
#[test]
fn diff_ast_match_name_pat_pairs_dotted_head() {
    assert_asts_match("match x with Foo.Bar (x = y) -> 1\n");
}

/// The named-field group composes with the pattern infix tail: `Case (a = 1) ::
/// t` is `ListCons(LongIdent(Case, NamePatPairs[…]), t)`, pinning that the
/// `NamePatPairs` arg group ends at the `)` and the `::` binds outside it.
#[test]
fn diff_ast_match_name_pat_pairs_cons() {
    assert_asts_match("match x with Case (a = 1) :: t -> 1\n");
}

/// The named-field group attaches to a dotted head across an indented
/// continuation line — `Foo.Bar⏎    (a = y)`. FCS treats the more-indented
/// `(a = y)` as the head's argument (verified: one `NamePatPairField`, no parse
/// error), and our detector's filtered-cursor guard keeps that working (the
/// continuation `(` is a real filtered token, not an offside-break virtual)
/// while refusing to mis-bump a layout virtual as the `(`.
#[test]
fn diff_ast_match_name_pat_pairs_dotted_head_continuation() {
    assert_asts_match("match x with\n| Foo.Bar\n    (a = y) -> 1\n");
}

/// A parenthesised operator name as a `match`-clause pattern — `match x with
/// (+) -> 0`. FCS reduces `(op)` through `opName → pathOp →
/// atomicPatternLongIdent` to the singleton-lowercase `SynPat.Named(SynIdent
/// op_Addition, OriginalNotationWithParen "+")`. Exercises the operator-name
/// pattern at a clause-head atomic position (a distinct call site from the
/// binding head): our `NAMED_PAT > [LPAREN_TOK, IDENT_TOK("+"), RPAREN_TOK]`
/// projects to `Named("+")`, matched against FCS's de-quoted spelling.
#[test]
fn diff_ast_match_operator_name_pattern() {
    assert_asts_match("match x with (+) -> 0\n");
}

// ---- Phase 11 error recovery: incomplete `match` -------------------------
//
// Mid-edit `match` states, each followed by a good decl that must survive (the
// offside `let y` closes the match). FCS recovers a missing clause result as
// `SynExpr.ArbitraryAfterError`, which projects to the shared
// `NormalisedExpr::Error` marker; for `match e with` with *no* clauses, FCS
// emits zero clauses, so the spurious empty clause our parser leaves at the
// `with` boundary (a `MATCH_CLAUSE` with no pattern) is dropped by the
// normaliser. `assert_asts_match_allow_errors` checks both sides error and
// agree on the recovered tree.

/// `match e with` and nothing after — FCS: `Match(e, [])` (zero clauses); the
/// trailing `let y = 2` survives as its own decl.
#[test]
fn diff_ast_match_recover_no_clauses() {
    assert_asts_match_allow_errors("let x = match e with\nlet y = 2\n");
}

/// A clause with a pattern but no result — `match e with A ->`. FCS:
/// `Match(e, [Clause(A, ArbitraryAfterError)])`; the result hole projects to
/// `Error`.
#[test]
fn diff_ast_match_recover_clause_missing_result() {
    assert_asts_match_allow_errors("let x = match e with A ->\nlet y = 2\n");
}

/// A complete clause then an incomplete final one — `match e with A -> 1 | B ->`.
/// Only the last clause's result is a hole (`Error`); the first is intact.
#[test]
fn diff_ast_match_recover_last_clause_missing_result() {
    assert_asts_match_allow_errors("let x = match e with A -> 1 | B ->\nlet y = 2\n");
}

/// A *guarded* clause with a missing result — `match e with A when cond ->`. The
/// guard `cond` is the clause's only `Expr` child, so the result must be
/// resolved keyword-relatively (after `->`, hence `None` → `Error`) rather than
/// as the trailing child, which would wrongly take `cond` as the result. FCS:
/// `Clause(A, when = cond, result = ArbitraryAfterError)`.
#[test]
fn diff_ast_match_recover_guarded_clause_missing_result() {
    assert_asts_match_allow_errors("let x = match e with A when cond ->\nlet y = 2\n");
}

/// The `function` (match-lambda) form shares the clause-list projection, so the
/// same empty-clause drop applies — `function` with no clauses → `[]`.
#[test]
fn diff_ast_function_recover_no_clauses() {
    assert_asts_match_allow_errors("let x = function\nlet y = 2\n");
}

/// `function` with a complete clause then an incomplete one — the same
/// result-hole recovery as `match`, via the shared clause-list projection.
#[test]
fn diff_ast_function_recover_last_clause_missing_result() {
    assert_asts_match_allow_errors("let x = function A -> 1 | B ->\nlet y = 2\n");
}

// ---- unparenthesised typed pattern in a match clause -----------------------
//
// FCS admits a bare typed pattern `pat : type` as a match-clause head
// (`pars.fsy` `patternClauses` → `pattern` → `... COLON typeWithTypeConstraints`).
// The type annotation is parsed *greedily* by the same
// `typeWithTypeConstraints` used inside parens, so it absorbs a following `->`
// as a function-type arrow: `| y: int -> e` therefore has NO clause arrow and
// FCS reports "expected `->`" — see the both-error unit guard in
// `src/parser/tests/patterns.rs`. The construct only *succeeds* when the type is bounded
// by a token the type grammar stops at — `::` (cons) or `as` — which is exactly
// the ubiquitous corpus shape `| h: Ident :: t ->`
// (`CheckRecordSyntaxHelpers.fs`, `CompilerOptions.fs`).

/// `| h: int :: t ->` — a typed pattern as the head of a `::`. The type `int`
/// stops at `::`, so the clause reads `ListCons(Typed(h, int), t)` and the `->`
/// remains the clause arrow (the `CheckRecordSyntaxHelpers.fs` shape).
#[test]
fn diff_ast_clause_typed_pat_before_cons() {
    assert_asts_match("let f x = match x with | h: int :: t -> 1 | _ -> 2\n");
}

/// `| opt: string :: t when g ->` — typed head of a `::`, then a `when` guard
/// (the `CompilerOptions.fs` shape). The guard sits *after* the cons, so the
/// type is still bounded by `::`.
#[test]
fn diff_ast_clause_typed_pat_before_cons_guarded() {
    assert_asts_match("let f x = match x with | h: string :: t when true -> 1 | _ -> 2\n");
}

/// `| y: int as z ->` — typed pattern bound by `as`. The type `int` stops at
/// `as`, giving `As(Typed(y, int), z)`.
#[test]
fn diff_ast_clause_typed_pat_before_as() {
    assert_asts_match("let f x = match x with | y: int as z -> 1 | _ -> 2\n");
}

/// `| h: int list :: t ->` — a *postfix* type application (`int list`) as the
/// annotation, still bounded by `::`. Confirms the type parser handles the
/// full type grammar in this position, not just atomic names.
#[test]
fn diff_ast_clause_typed_pat_postfix_type_before_cons() {
    assert_asts_match("let f x = match x with | h: int list :: t -> 1 | _ -> 2\n");
}

/// Negative (recovered) guard: `| y: int -> e` — the type annotation greedily
/// absorbs `-> e` as a function type (`Typed(y, Fun(int, e))`), leaving the
/// clause with no arrow. FCS reports "expected `->`"; we error too. Pinned as a
/// both-error recovery diff so the greedy boundary stays aligned with FCS.
#[test]
fn diff_ast_clause_typed_pat_direct_arrow_both_error() {
    assert_asts_match_allow_errors("let f x = match x with | y: int -> 1\n");
}

// ---- match scrutinee is a `typedSequentialExpr` --------------------------
//
// FCS parses the `match <scrutinee> with` head as a `typedSequentialExpr`
// (`pars.fsy`), so the scrutinee may carry a trailing `: type` annotation
// (`SynExpr.Typed`) or be a `;`-sequential (`SynExpr.Sequential`) — not just a
// single expression. `match e : t with` and `match e1; e2 with` are the corpus
// shapes (`FSharpCheckerResults`-style typed scrutinees; `DiscriminatedUnionType`
// sequential scrutinee).

/// A type-annotated match scrutinee (`match e : t with`) → `Match(Typed(e, t), …)`.
#[test]
fn diff_ast_match_typed_scrutinee() {
    assert_asts_match("let f x = match x : int with _ -> 1\n");
}

/// A type-annotated scrutinee with a postfix type application (`: int option`).
#[test]
fn diff_ast_match_typed_scrutinee_app() {
    assert_asts_match("let f x = match x : int option with _ -> 1\n");
}

/// A `;`-sequential match scrutinee (`match e1; e2 with`) →
/// `Match(Sequential(e1, e2), …)`.
#[test]
fn diff_ast_match_sequential_scrutinee() {
    assert_asts_match("let f x = match ignore x; x with _ -> 1\n");
}

/// A `match!` (computation-expression) scrutinee is the same `typedSequentialExpr`.
#[test]
fn diff_ast_match_bang_typed_scrutinee() {
    assert_asts_match("let f x = async {\n    match! x : int with\n    | _ -> return 1\n}\n");
}
