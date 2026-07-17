//! Typed/untyped quotations `<@ ... @>` and their closers.

use crate::common::assert_filtered_streams_match;

/// Typed quotation `<@ … @>`. Exercises `CtxtParen(Opener::Quote)`: `<@` pushes
/// the scope, `@>` arrives as `TokenRExprParen`, force-closes any inner
/// SeqBlock/CtxtFun contexts (emitting their closing tokens at `@>`'s span),
/// then pops CtxtParen. `@>` itself is emitted — unlike RPAREN/RBRACE it is not
/// swallowed by FCS's outer wrapper. The balance check is tag-sensitive:
/// `LQUOTE q1, RQUOTE q2 when q1 = q2` (LexFilter.fs:425), so `<@` never
/// closes with `@@>`.
#[test]
fn diff_filtered_typed_quotation() {
    assert_filtered_streams_match("let f = <@ fun x -> x @>\n");
}

/// Untyped (raw) quotation `<@@ … @@>`. Same mechanism as the typed form but
/// uses `Opener::QuoteRaw` so it only closes with `@@>` and not `@>`.
#[test]
fn diff_filtered_raw_quotation() {
    assert_filtered_streams_match("let f = <@@ fun x -> x @@>\n");
}

/// Quotation close immediately followed by `.` — `<@ 1 @>.Raw`. The raw lexer
/// produces `Token::RQuoteDot` (matching the full `@>.` to beat `Op("@>.")`);
/// the lexfilter splits it into `Token::RQuote` + `Token::Dot` before dispatch,
/// mirroring FCS's `RQUOTE_DOT` → `(RQUOTE, DOT)` split at LexFilter.fs:2687.
#[test]
fn diff_filtered_quotation_dot() {
    assert_filtered_streams_match("let f = <@ 1 @>.Raw\n");
}

/// Quotation close inside an anonymous-record expression: `{| F = <@ 1 @>|}`.
/// The raw lexer produces `Token::RQuoteBarRBrace`; the lexfilter splits it into
/// `Token::RQuote` + `Token::BarRBrace` with FCS's overlapping spans:
/// RQUOTE=[start, end-2), BAR_RBRACE=[start+1, end) (LexFilter.fs:2756-2757).
#[test]
fn diff_filtered_quotation_bar_rbrace() {
    assert_filtered_streams_match("let r = {| F = <@ 1 @>|}\n");
}

/// Multiline quotation body with two aligned statements. FCS pushes
/// `CtxtSeqBlock(NoAddBlockEnd)` for every `TokenLExprParen` opener including
/// `LQUOTE` (LexFilter.fs:2281-2288), so the inner `let x = 1` and `x` get
/// an `OffsideBlockSep` between them.
#[test]
fn diff_filtered_quotation_multiline_body() {
    assert_filtered_streams_match("let q = <@\n    let x = 1\n    x\n@>\n");
}
