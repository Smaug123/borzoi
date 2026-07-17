//! Differential test (`parser::parse` vs FCS): the general offside/indentation
//! FS0058 (`lexfltTokenIsOffsideOfContextStartedEarlier`) emitted by the
//! lex-filter when a context is pushed left of its undentation limit
//! (`docs/offside-diagnostics-plan.md`, §A). These are *recoverable* — FCS
//! produces the same tree it would without the offside, then adds the FS0058 —
//! so [`assert_asts_match_with_diagnostic`] pins that our normalised tree agrees
//! with FCS's recovery **and** that we flag the same byte span with an error.
//!
//! Our parse default is F# 10, where strict-indentation makes FS0058 an *error*
//! (so FCS's `ParseHadErrors` is set and the recovery tree is still emitted).

use crate::common::{
    assert_asts_match, assert_asts_match_at_langversion, assert_asts_match_with_diagnostic,
    assert_offside_spans_match, assert_offside_spans_match_at_langversion,
};
use borzoi_cst::language_version::LanguageVersion;

/// A record type with a trailing `with` and no augment members
/// (`type R = { X: int } with` at EOF). FCS opens a `with`-augment body block
/// whose anchor is the EOF (line 2, column 0) — offside of the `with` context —
/// and reports FS0058 there while still recovering the record shape. Our
/// lex-filter now emits the matching FS0058 at that same EOF offset.
#[test]
fn diff_ast_offside_trailing_with_on_record() {
    assert_asts_match_with_diagnostic("type R = { X: int } with\n", 58);
}

/// Same trailing-`with` offside, but the `with` sits on its own line indented
/// past the record — the augment body is still empty, so FCS anchors the
/// FS0058 at the EOF offside of the `with`.
#[test]
fn diff_ast_offside_trailing_with_on_own_line() {
    assert_asts_match_with_diagnostic("type R = { X: int }\n    with\n", 58);
}

/// A `type` with one member followed by a trailing `with` and no augment
/// members. FCS recovers the member-holding type and flags the empty augment
/// body's offside FS0058.
#[test]
fn diff_ast_offside_member_trailing_with() {
    assert_asts_match_with_diagnostic("type T =\n    member _.M = 1\n    with\n", 58);
}

/// A trailing `with` on an `exception` definition with no augment members —
/// the `CtxtException`-anchored augment body is empty, so FCS reports the
/// offside FS0058 at EOF.
#[test]
fn diff_ast_offside_exception_trailing_with() {
    assert_asts_match_with_diagnostic("exception E with\n", 58);
}

/// A bare wildcard `*` as a `let` RHS head (`let r = *`): the `*` opens no
/// context, so the RHS block anchors at EOF, offside of the `let`. FCS reports
/// FS0058 there and recovers `IndexRange(None, None)` — a tree our parser
/// matches — so this is a full offside match (span + recovery). (The `-`/`&`/
/// `upcast` prefix-operand siblings emit the same FS0058 but recover to a
/// different tree; those are pinned diagnostic-only in
/// `parser_diff_ranges::bare_head_wildcard_flags_offside_at_eof`.)
#[test]
fn diff_ast_offside_bare_wildcard_let_rhs() {
    assert_asts_match_with_diagnostic("let r = *\n", 58);
}

/// A `do` body inside a `type` body that fails to indent past the `do`:
/// FCS's undentation arm `CtxtSeqBlock(First) :: CtxtDo :: CtxtSeqBlock ::
/// (CtxtTypeDefns | CtxtModuleBody)` (LexFilter.fs:779) limits the do-body
/// block to `do`'s column + 1, so a body aligned *at* the `do` is offside.
/// FCS reports FS0058 anchored at the body's first token (`printfn`).
#[test]
fn diff_offside_do_body_aligned_with_do_under_type() {
    assert_offside_spans_match("type C =\n    do\n    printfn \"x\"\n");
}

/// Same undentation arm with `CtxtModuleBody` as the bottom context: a
/// nested module's `do` with its body aligned at the `do` column.
#[test]
fn diff_offside_do_body_aligned_with_do_under_module() {
    assert_offside_spans_match("module M =\n    do\n    printfn \"x\"\n");
}

/// A `type` definition illegally nested in a parenthesised `let` RHS. FCS
/// flags FS0058 at `type` (pushed left of the paren's limit) but — because
/// the `=`-driven `CtxtTypeDefns` replacement goes through
/// `replaceCtxtIgnoreIndent` (LexFilter.fs:2228) — emits **no** FS0058 at
/// the `=`. Pins that our replacement is likewise indent-exempt: the span
/// *sets* must agree, so an extra `=`-anchored diagnostic fails.
#[test]
fn diff_offside_type_equals_replacement_is_indent_exempt() {
    assert_offside_spans_match("let outer = (\ntype Inner = int\n)\n");
}

/// Match-lambda clauses aligned with the enclosing `let` — the common
/// dedented style FCS's L815 arm (`CtxtMatchClauses, CtxtFunction ::
/// CtxtSeqBlock :: CtxtLetDecl` → `let`'s column, precisely) exists to
/// accept. Must parse cleanly with **no** FS0058: the specific arm has to
/// fire before the generic `CtxtFunction :: rest` no-limit recursion (FCS
/// L920), which would otherwise skip through to the LetDecl catch-all's
/// `let.col + 1` and flag the leading `|`.
#[test]
fn diff_ast_function_clauses_aligned_with_let_no_offside() {
    assert_asts_match("let f = function\n| A -> 1\n| B -> 2\n");
}

/// Same shape nested in a module body, clauses aligned with the indented
/// `let` (column 4): still clean, still no FS0058.
#[test]
fn diff_ast_function_clauses_aligned_with_nested_let_no_offside() {
    assert_asts_match("module M =\n    let f = function\n    | A -> 1\n    | B -> 2\n");
}

/// The do-under-type offside with a non-ASCII comment shifting the `do`
/// rightwards: `é` is 2 UTF-8 bytes but 1 UTF-16 unit, so the limiting
/// context's byte column (13) and FCS's reported column (12, printed
/// 1-based as 13) disagree. Pins that the position embedded in our FS0058
/// message counts UTF-16 code units from the line start, as FCS does —
/// not raw bytes.
#[test]
fn diff_offside_message_position_counts_utf16_units() {
    assert_offside_spans_match("type C =\n    (* \u{e9} *) do\n    printfn \"x\"\n");
}

/// Below F# 8 the strict-indentation gate is *off*, so an offside push is
/// **kept** (with a warning) rather than aborted. For `module Foo =` followed by
/// a column-0 `let`, FCS at 7.0 warns yet nests `z` *inside* `Foo`; aborting the
/// module-body SeqBlock push (as the strict path does at 8.0+) would instead
/// leave `z` a sibling. Pins that the push decision honours the resolved
/// strict-indentation boolean, not a hardcoded strict push — so the tree agrees
/// with FCS at 7.0. (At the default 10.0 the same source errors and `z` *is* a
/// sibling; that divergence is version-correct.)
#[test]
fn diff_ast_offside_v7_keeps_module_body_push() {
    assert_asts_match_at_langversion("module Foo =\nlet z = 1\n", LanguageVersion::V7_0, "7.0");
}

/// A property accessor whose body deindents left of `with` but stays right of
/// the `member` keyword. FCS limits the accessor body by the *member*'s column
/// (`CtxtWithAsLet :: CtxtMemberHead` → member.col + 1), not the `with`'s own
/// column, so `member` at col 4 / `with` at col 15 / body at col 7 is clean.
/// Without the member-accessor undentation arm we fall through to the generic
/// `WithAsLet` limit (with.col + 1 = 16) and emit a spurious FS0058 at the body.
#[test]
fn diff_ast_offside_accessor_body_deindented_past_with() {
    assert_asts_match("type C =\n    member _.P with get () =\n       1\n");
}

/// An inactive `#if` region (five lines, dropped from the active stream) sits
/// above an offside `do`-under-type. FCS reports the limiting-context position
/// counting the skipped lines — `(7:5)`, the real line of `do` — while the
/// squiggle spans the absolute bytes of `printfn`. Pins that our line cursor
/// advances across filtered directive / inactive-code gaps, so the FS0058
/// message embeds the *real* context line rather than one short by the region's
/// height.
#[test]
fn diff_offside_inactive_region_shifts_context_line() {
    assert_offside_spans_match(
        "#if FOO\naaa\nbbb\nccc\n#endif\ntype C =\n    do\n    printfn \"x\"\n",
    );
}

/// A trailing `with` on a record, followed by an inactive `#if` region that runs
/// to EOF. FCS anchors the empty-augment-body offside at the *true* end of file
/// (past the inactive region), so the FS0058 squiggle sits at `source.len()`.
/// Pins that our synthetic EOF advances across the trailing filtered gap rather
/// than stopping at the last active token before it.
#[test]
fn diff_offside_trailing_inactive_region_eof_anchor() {
    assert_offside_spans_match("type R = { X: int } with\n#if FOO\ndead\n#endif\n");
}

// --- EOF-anchored offside (FCS's `startPosOfTokenTup` col−1 rule) ---------
//
// When the anchoring token is the synthetic EOF, FCS treats it as column −1
// ("processed as if on column -1 … forces the closure of all contexts",
// LexFilter.fs:640), so a context pushed at EOF is offside of a col-0 enclosing
// limit and FCS emits FS0058 (error at 8.0+, warning at 7.0). See
// `docs/completed/offside-eof-column-minus-one-plan.md`.

/// `let f = function` with no clauses: FCS anchors the `CtxtMatchClauses` at EOF
/// (col −1), left of the enclosing `let` (col 0), and reports FS0058 at
/// `source.len()`. The FUNCTION push is unconditional, so nothing aborts — only
/// the diagnostic appears. Default 10.0 ⇒ error.
#[test]
fn diff_offside_function_no_clauses_at_eof() {
    assert_offside_spans_match("let f = function\n");
}

/// Same, at 7.0 ⇒ the FS0058 is a warning (FCS `ParseHadErrors` may still be set
/// by the companion FS0010, but the offside diagnostic itself is a warning).
#[test]
fn diff_offside_function_no_clauses_at_eof_v7() {
    assert_offside_spans_match_at_langversion("let f = function\n", LanguageVersion::V7_0, "7.0");
}

/// `match x with` at EOF: FCS pushes the `CtxtMatchClauses` at the EOF lookahead
/// (col −1) and reports FS0058 at `source.len()`. Our `with_dispatch` EOF guard
/// currently skips the push entirely, missing the diagnostic.
#[test]
fn diff_offside_match_no_clauses_at_eof() {
    assert_offside_spans_match("match x with\n");
}

/// `try x with` at EOF — the `CtxtTry` analogue of the `match` case, same
/// EOF-anchored `CtxtMatchClauses` push and FS0058.
#[test]
fn diff_offside_try_no_clauses_at_eof() {
    assert_offside_spans_match("try x with\n");
}

/// `match x with` at 7.0 ⇒ warning.
#[test]
fn diff_offside_match_no_clauses_at_eof_v7() {
    assert_offside_spans_match_at_langversion("match x with\n", LanguageVersion::V7_0, "7.0");
}

/// Regression guard for the `limit ≥ 1` inertness claim: a `let`/`module` whose
/// body is absent at EOF is limited by `col + 1 ≥ 1`, so it already emits FS0058
/// with the anchor at col 0 (`0 < 1`) — the EOF col−1 flip must **not** perturb
/// these (they are unchanged before and after). Kept as a guard so a regression
/// in the EOF handling that double-emits or shifts these fails loudly.
#[test]
fn diff_offside_let_body_absent_at_eof_unchanged() {
    assert_offside_spans_match("let x =\n");
}

#[test]
fn diff_offside_module_body_absent_at_eof_unchanged() {
    assert_offside_spans_match("module M =\n");
}
