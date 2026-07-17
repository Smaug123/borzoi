//! `function` clauses and `exception` declarations.

use crate::common::assert_filtered_streams_match;

/// `let f = function | 1 -> "one" | _ -> "other"` — single-line
/// `function` as RHS of a let. FCS pushes `CtxtFunction(<function-pos>)`
/// and `CtxtMatchClauses(leadingBar=false, <lookahead-pos>)`, emitting
/// `OFUNCTION` (LexFilter.fs:2469-2475). `leadingBar` is false because
/// the next token after `function` is `1`, not `|`. All clauses live on
/// the same line, so neither context closes via offside.
#[test]
fn diff_filtered_function_single_line() {
    assert_filtered_streams_match("let f = function | 1 -> \"one\" | _ -> \"other\"\n");
}

/// `let f = function\n    | 1 -> "one"\n    | _ -> "other"\n` —
/// multiline with clauses indented past the let. `leadingBar=true` here
/// (next token after `function` is `|`). MatchClauses anchors at the
/// first `|`'s column.
#[test]
fn diff_filtered_function_multiline_indented() {
    assert_filtered_streams_match("let f = function\n    | 1 -> \"one\"\n    | _ -> \"other\"\n");
}

/// `let f = function\n| 1 -> ...\n| _ -> ...\n` — clauses dedented to
/// the let's column. Pins the L815/L920 undentation_limit arms: the
/// MatchClauses push under `function` is limited by the enclosing
/// CtxtLetDecl's column (L815), so column-0 clauses are accepted even
/// though `function` itself sits at col 8.
#[test]
fn diff_filtered_function_multiline_dedented() {
    assert_filtered_streams_match("let f = function\n| 1 -> \"one\"\n| _ -> \"other\"\n");
}

/// `match` with a `function` in one of its arms — exercises stack
/// interaction (CtxtMatchClauses :: CtxtMatch above, then a fresh
/// CtxtFunction :: CtxtMatchClauses pushed for the inner function).
#[test]
fn diff_filtered_function_inside_match_arm() {
    assert_filtered_streams_match("let g x = match x with _ -> function _ -> 0\n");
}

/// `let f = function _ ->\n    0\n` — the arm body deindented below the
/// pattern. FCS closes MatchClauses (anchored at the underscore col 17)
/// before the body token at col 4, emitting OffsideRightBlockEnd +
/// OffsideEnd, so the `0` lands *outside* the function. Pins the L815
/// keying: the let-column relaxation must fire only when MatchClauses
/// itself is being pushed, not for every push beneath it (otherwise the
/// `->` SeqBlock inherits the let column and swallows the offside body).
#[test]
fn diff_filtered_function_body_deindented_below_pattern() {
    assert_filtered_streams_match("let f = function _ ->\n    0\n");
}

/// `exception Foo\n` — bare exception declaration. Push site at FCS
/// LexFilter.fs:2135-2141. `CtxtException` is silent (endTokenForACtxt
/// returns None) and offside-pops at the next token at column ≤ the
/// `exception` keyword's column (FCS LexFilter.fs:1990). At EOF the
/// context closes via the force-closure path, but because Exception's
/// end token is None the closure emits no virtual — only token spans
/// already produced by the lex run, so the EOF-span divergence noted
/// for Function does not apply here.
#[test]
fn diff_filtered_exception_bare() {
    assert_filtered_streams_match("exception Foo\n");
}

/// `exception Foo of int * string\n` — exception with a payload type.
/// Same context structure as bare; pins that the `of` token does not
/// disturb the Exception context.
#[test]
fn diff_filtered_exception_of_payload() {
    assert_filtered_streams_match("exception Foo of int * string\n");
}

/// `exception Foo with\n  member _.M() = 1\n` — exception augmented
/// with a member. Exercises the WITH-as-augment dispatch at FCS L2362
/// for Exception. FCS reaches the `_ ->` arm at L2424 (since the
/// lookahead `member` is a keyword, not IDENT/RBRACE/PUBLIC/…), which
/// pushes `CtxtWithAsAugment(Exception.col)` and a SeqBlock and
/// returns raw `WITH`. The augment column is then refined by the L877
/// limit-rule. Without CtxtException, Rust falls into the L2462
/// catch-all and also emits raw WITH; the token stream matches
/// because the L902 undentation-limit recursion folds the augment's
/// limit back to the outer SeqBlock's column anyway. This test pins
/// that equivalence as a regression guard.
#[test]
fn diff_filtered_exception_with_augment() {
    assert_filtered_streams_match("exception Foo with\n  member _.M() = 1\n");
}

/// `exception Foo with\n  foo = 1\n` — exception WITH whose lookahead
/// token is an `IDENT` (rather than a keyword like `member`). FCS
/// L2362 fires (Exception is head), the lookahead matches the
/// `RBRACE | IDENT | PUBLIC | …` arm at L2367, and FCS emits
/// `OWITH` (-> `OffsideWith` in `FSharpTokenKind`) plus a
/// `CtxtWithAsLet` anchored at Exception's column. This is the
/// genuine observable divergence the slice exists for: without
/// `CtxtException`, Rust falls into the L2462 WITH catch-all and
/// emits raw `With`, never `OffsideWith`.
#[test]
fn diff_filtered_exception_with_ident_emits_offside_with() {
    assert_filtered_streams_match("exception Foo with\n  foo = 1\n");
}

/// `exception J' of r : (\n    int\n) with\n    member _.A(\n        _\n    ) = ()\n`
/// — multi-line exception with a paren-wrapped payload and member
/// augment, drawn from the FCS conformance suite
/// (`tests/.../OffsideExceptions/RelaxWhitespace2.fs`). Exercises
/// Exception staying on the stack across an inner CtxtParen (the
/// `(int)`), then the WITH dispatch + member body. Pinned as a
/// regression test for the augment path.
#[test]
fn diff_filtered_exception_paren_payload_with_member() {
    assert_filtered_streams_match(
        "exception J' of r : (\n    int\n) with\n    member _.A(\n        _\n    ) = ()\n",
    );
}

/// `module M =\n  exception Foo\n  let x = 1\n` — exception nested in a
/// module body. The exception's offside pop (FCS L1990) must close
/// CtxtException before the `let` token at the same column, so the
/// `let` belongs to the surrounding ModuleBody / SeqBlock rather than
/// the exception's scope.
#[test]
fn diff_filtered_exception_inside_module_body() {
    assert_filtered_streams_match("module M =\n  exception Foo\n  let x = 1\n");
}

/// `exception Foo\nlet x = 1\n` — exception at file head, followed by a
/// `let` at the same column. Pins the offside-pop arm: the `let` token
/// at column 0 closes the Exception context (whose offside column is 0
/// too: `tokenStartCol <= offsidePos.Column`), then the let participates
/// in the outer SeqBlock.
#[test]
fn diff_filtered_exception_then_let_same_column() {
    assert_filtered_streams_match("exception Foo\nlet x = 1\n");
}
