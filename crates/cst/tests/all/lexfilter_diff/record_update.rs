//! `{ r with ... }` record-update brace-shape dispatch.

use crate::common::assert_filtered_streams_match;

/// Single-line record-update `{ r with A = 1 }`. The brace-shape WITH
/// dispatch (FCS LexFilter.fs:2363) pushes CtxtWithAsLet, emits OWITH,
/// and the closing `}` force-closes CtxtWithAsLet emitting OEND. Pre-
/// port the inner `with` was passed through as a raw With and no OEND
/// was emitted before the surrounding OffsideDeclEnd. (#16)
#[test]
fn diff_filtered_record_update_single_line() {
    assert_filtered_streams_match("let r2 = { r with A = 1 }\n");
}

/// Semicolon-separated multi-binding record update on a single line:
/// `{ r with A = 1; B = 2 }`. Inner SeqBlock(NoAddBlockEnd) pushed by
/// the WITH dispatch covers both bindings; the `;` punctuates them
/// without emitting an OffsideBlockSep (FCS uses NoAddBlockEnd here
/// per the comment at LexFilter.fs:2256-2259). (#16)
#[test]
fn diff_filtered_record_update_semicolon() {
    assert_filtered_streams_match("let r2 = { r with A = 1; B = 2 }\n");
}

/// Multi-line multi-binding record update with the bindings aligned
/// past the `{`'s column. Exercises the EQUALS+CtxtVanilla(true)+
/// CtxtSeqBlock+CtxtWithAsLet arm (FCS LexFilter.fs:2253-2263): the
/// second binding lands on a new line aligned with the first, the
/// inner Vanilla/SeqBlock pop, and the outer SeqBlock emits
/// OffsideBlockSep before the second `Identifier`. (#16)
#[test]
fn diff_filtered_record_update_multi_line() {
    assert_filtered_streams_match("let r2 =\n    { r with\n        A = 1\n        B = 2 }\n");
}

/// Anonymous-record update `{| r with A = 1 |}` (BraceBar opener).
/// Same brace-shape dispatch arm fires for `LBRACE_BAR` (FCS includes
/// both `LBRACE _ | LBRACE_BAR` in LexFilter.fs:2363). The closing
/// `|}` emits the same OEND-then-BarRBrace shape as a plain record
/// update. (#16)
#[test]
fn diff_filtered_record_update_anon() {
    assert_filtered_streams_match("let r2 = {| r with A = 1 |}\n");
}

/// Qualified-field record update `{ r with M.A = 1 }`. Forces the
/// `isLongIdentEquals` DOT-walk inside the lookahead (FCS
/// LexFilter.fs:1336-1344): `M . A =` is a long-ident-equals, so the
/// WITH dispatch still pushes the inner SeqBlock(NoAddBlockEnd) and
/// the CtxtVanilla pushed for `M` carries `is_long_ident_equals=true`
/// so the EQUALS arm can fire. (#16)
#[test]
fn diff_filtered_record_update_qualified_field() {
    assert_filtered_streams_match("let r2 = { r with M.A = 1 }\n");
}

/// Record-update inside a match arm body: `match x with | _ -> { r
/// with A = 1 }`. Regression guard for the previous defensive shim
/// era: the inner `with` must dispatch as OWITH for CtxtWithAsLet and
/// must NOT force-close the surrounding CtxtMatchClauses. The
/// brace-shape balance arm at `token_balances_head_context`
/// (LexFilter.fs:1268) keeps force-closure away from the match;
/// the brace-shape dispatch (LexFilter.fs:2363) then emits OWITH.
/// (#16)
#[test]
fn diff_filtered_with_inside_match_arm_record() {
    assert_filtered_streams_match("let f x = match x with | _ -> { r with A = 1 }\n");
}

/// Record-update with no closing `}`, followed by an offside-aligned
/// `let` on the next line. The CtxtWithAsLet offside-pop
/// (LexFilter.fs:2019) is the only path that emits OEND here — force-
/// closure on `}` never fires because there is no `}`. Without the
/// dedicated offside-pop arm, the WithAsLet would linger on the stack
/// and the second `let` would pass through with the wrong virtual
/// scaffolding. (#18)
#[test]
fn diff_filtered_record_update_unclosed_then_offside_let() {
    assert_filtered_streams_match("let x = { r with A = 1\nlet y = 0\n");
}

/// Record-update with quoted (backtick) field names. FCS's `IDENT _` pattern
/// in `isLongIdentEquals` (LexFilter.fs:1327-1351) covers both regular and
/// quoted identifiers; our lexer splits them into `Token::Ident` and
/// `Token::QuotedIdent` so the helper must accept both. Without that, the
/// inner record-binding SeqBlock isn't pushed and the OBLOCKSEP between
/// multi-line bindings is missing. (#16, codex)
#[test]
fn diff_filtered_record_update_quoted_field_multi() {
    assert_filtered_streams_match(
        "let r2 = { r with\n              ``A`` = 1\n              ``B`` = 2 }\n",
    );
}

/// Same-line record-update binding whose RHS opens with `use`. FCS's
/// `isControlFlowOrNotSameLine` (LexFilter.fs:1319-1324) lookahead pattern
/// is `TRY | MATCH | MATCH_BANG | IF | LET _ | FOR | WHILE | WHILE_BANG`,
/// and in FCS `LET _` matches both `let` (`LET(false)`) and `use`
/// (`LET(true)` per LexHelpers.fs:336-362). Our lexer splits these into
/// `Token::Let` and `Token::Use`, so both must be listed. Without `Use` the
/// EQUALS arm picks `NoAddBlockEnd` and we drop the `OffsideBlockBegin /
/// OffsideEnd` pair FCS emits around the `use`-binding RHS. (#16, codex)
#[test]
fn diff_filtered_record_update_use_rhs_same_line() {
    assert_filtered_streams_match("let r2 = { r with A = use y = x in y }\n");
}

/// EQUALS immediately followed by EOF inside a record-update: the strict
/// undentation push of the inner SeqBlock at the lookahead position fails
/// (synthetic EOF can't anchor a CtxtSeqBlock at WithAsLet's required
/// `col + 1`), so FCS's `pushCtxtSeqBlockAt` falls back to the trigger
/// token (the EQUALS) and emits OBLOCKBEGIN at its position. Without the
/// fallback path in `push_ctxt_seq_block` we instead push at the EOF
/// position, drifting the OBLOCKBEGIN range. (#10)
#[test]
fn diff_filtered_record_update_equals_then_eof() {
    assert_filtered_streams_match("let x = { r with A =\n");
}

/// Record-update with a multi-line binding RHS: `{ r with A =\n    f x }`.
/// The EQUALS+CtxtVanilla+CtxtWithAsLet arm (LexFilter.fs:2253-2254) is the
/// one that opens an inner SeqBlock(NoAddBlockEnd) for the RHS expression so
/// continuation lines align under `f`. Without that arm we'd emit the wrong
/// virtuals around the wrapped RHS. (#17)
#[test]
fn diff_filtered_record_update_wrapped_rhs() {
    assert_filtered_streams_match("let r2 = { r with A =\n                    f x }\n");
}
