//! Pre-dispatch `hw_token_fetch` rules: token rewriting that runs before any
//! context handler. See `super::Filter::hw_token_fetch` for the dispatch order.

use super::{Filter, Pos, Step, TokenContent, TokenTup, Virtual};
use crate::lexer::{LexError, Span, Token};

impl<'a, I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>> Filter<'a, I> {
    /// Pre-dispatch token rewriting: the no-processing passthrough, compound
    /// quote-close splitting, high-precedence app injection, head-context
    /// force-closure, and the empty-stack `;;` reset.
    pub(super) fn predispatch(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // FCS's `tokensThatNeedNoProcessingCount > 0` guard
        // (LexFilter.fs:1644-1646). Tokens queued via
        // `delay_token_no_processing` (synthetic ENDs from the
        // multi-member cascade, saved trigger keywords) flow straight
        // back to the parser. We must check this BEFORE the
        // RQUOTE_DOT split and any other dispatch logic — FCS places
        // the guard as the first arm of its dispatch match.
        if self.tokens_that_need_no_processing > 0 {
            self.tokens_that_need_no_processing -= 1;
            if matches!(&tt.token, TokenContent::Real(_)) {
                self.last_real_end = tt.span.end;
            }
            return Step::Emit(tt);
        }

        // Split compound quote-close tokens before any rule dispatch.
        // FCS's lex.fsl produces RQUOTE_DOT for `@>.` and
        // RQUOTE_BAR_RBRACE for `@>|}` so the Op regex doesn't absorb
        // the suffix; LexFilter.fs:2687-2691 and 2754-2758 split them.
        // We match 3/4-char tokens to beat `Op` (greedy longest-match),
        // then split here into the quote-close + suffix pair.
        // LIFO: delay the suffix first so the close comes out first.
        //
        // Span arithmetic mirrors FCS exactly:
        //   RQUOTE_DOT:        RQUOTE=[start, end-1), DOT=[end-1, end)
        //   RQUOTE_BAR_RBRACE: RQUOTE=[start, end-2), BAR_RBRACE=[start+1, end)
        //                      (FCS UseShiftedLocation(..., 1, 0) for BAR_RBRACE)
        if let TokenContent::Real(
            Token::RQuoteDot
            | Token::RQuoteRawDot
            | Token::RQuoteBarRBrace
            | Token::RQuoteRawBarRBrace,
        ) = &tt.token
        {
            let is_bar = matches!(
                &tt.token,
                TokenContent::Real(Token::RQuoteBarRBrace | Token::RQuoteRawBarRBrace)
            );
            let is_raw = matches!(
                &tt.token,
                TokenContent::Real(Token::RQuoteRawDot | Token::RQuoteRawBarRBrace)
            );
            let close_tok = if is_raw {
                Token::RQuoteRaw
            } else {
                Token::RQuote
            };
            let suffix_tok = if is_bar { Token::BarRBrace } else { Token::Dot };
            // RQUOTE ends: 1 byte before end for DOT cases (LexFilter.fs:2690
            // `UseShiftedLocation(..., 0, -1)`), 2 bytes for BAR_RBRACE cases
            // (LexFilter.fs:2757 `UseShiftedLocation(..., 0, -2)`).
            let close_trim = if is_bar { 2 } else { 1 };
            let close_end_byte = tt.span.end - close_trim;
            let close_end_col = tt.end.col - close_trim as u32;
            // DOT suffix starts at close_end; BAR_RBRACE starts at start+1.
            let suffix_start_byte = if is_bar {
                tt.span.start + 1
            } else {
                close_end_byte
            };
            let suffix_start_col = if is_bar {
                tt.start.col + 1
            } else {
                close_end_col
            };
            let suffix_tt = TokenTup {
                token: TokenContent::Real(suffix_tok),
                span: suffix_start_byte..tt.span.end,
                start: Pos {
                    line: tt.start.line,
                    col: suffix_start_col,
                },
                end: tt.end,
            };
            let close_tt = TokenTup {
                token: TokenContent::Real(close_tok),
                span: tt.span.start..close_end_byte,
                start: tt.start,
                end: Pos {
                    line: tt.end.line,
                    col: close_end_col,
                },
            };
            self.delay_token(suffix_tt); // pushed first → comes out second
            self.delay_token(close_tt); //  pushed second → comes out first
            return Step::Restart;
        }

        // Split `INT32_DOT_DOT` (`1..`) into `INT32` + `DOT_DOT` so range
        // specifications (`1..10`, `arr.[2..]`) reach the parser as a plain
        // integer literal followed by the `..` operator. The lexer fuses the
        // two (regex `[0-9][0-9_]*\.\.`) only to stop the float regex eating
        // `1.` as `Float64`; once lexed, the float ambiguity is resolved and
        // the fused token can be split back. FCS does the identical split in
        // its LexFilter (LexFilter.fs:2680-2684, `ShiftColumnBy(-2)`), so this
        // closes a known divergence in our post-filter stream.
        //
        // Span arithmetic: the `..` is always the trailing 2 ASCII bytes, and
        // an `INT32_DOT_DOT` never spans a line, so the split point is `end-2`
        // in both byte offset and column. LIFO: delay `DOT_DOT` first so the
        // `INT32` comes out first (matching `1..10` → `INT32 / DOT_DOT / INT32`).
        if let TokenContent::Real(Token::IntDotDot(text)) = &tt.token {
            let int_text = &text[..text.len() - 2];
            let split_byte = tt.span.end - 2;
            let split_col = tt.end.col - 2;
            let split_pos = Pos {
                line: tt.end.line,
                col: split_col,
            };
            let dotdot_tt = TokenTup {
                token: TokenContent::Real(Token::DotDot),
                span: split_byte..tt.span.end,
                start: split_pos,
                end: tt.end,
            };
            let int_tt = TokenTup {
                token: TokenContent::Real(Token::Int(int_text)),
                span: tt.span.start..split_byte,
                start: tt.start,
                end: split_pos,
            };
            self.delay_token(dotdot_tt); // pushed first → comes out second
            self.delay_token(int_tt); //   pushed second → comes out first
            return Step::Restart;
        }

        // Split `..^` (`DOT_DOT_HAT`) into `DOT_DOT` + the `^` prefix
        // (`Op("^")`), so a from-end slice (`arr.[..^1]`, `let x = ..^1`) reaches
        // the parser as the open-lower range operator `..` followed by the
        // ordinary from-end prefix `^` — no special-casing in the range/index
        // productions, and the precedence of `..^a + b` (`.. (^a + b)`) falls out.
        // FCS does the identical split (LexFilter.fs:2672-2675, `ShiftColumnBy
        // (-1)`). The `^` is the trailing ASCII byte; `..^` never spans a line.
        // LIFO: delay `^` first so the `DOT_DOT` comes out first.
        //
        // Guard: split only when the `..^` is a genuine from-end slice, i.e. the
        // next source byte is *not* an operator-continuation char. When it is
        // (`..^+1`, `..^^1`, `..^-1`), FCS's maximal-munch lexer takes the whole
        // contiguous run as one `INFIX_AT_HAT_OP` and rejects it (FS1208); our
        // longest-match lexer instead split off `..^` already, so leave that token
        // unsplit — the parser has no production for a bare `DOT_DOT_HAT`, so it
        // errors, matching FCS rather than accepting `.. (^ (+1))`.
        if let TokenContent::Real(Token::DotDotHat) = &tt.token
            && !self.source.as_bytes().get(tt.span.end).is_some_and(|b| {
                matches!(
                    b,
                    b'!' | b'$'
                        | b'%'
                        | b'&'
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'/'
                        | b'<'
                        | b'='
                        | b'>'
                        | b'?'
                        | b'@'
                        | b'^'
                        | b'|'
                        | b'~'
                        // `:` continues a symbolic operator after the first char
                        // (`..^:`, `..^::`), so it too keeps `..^` fused.
                        | b':'
                )
            })
        {
            let split_byte = tt.span.end - 1;
            let split_col = tt.end.col - 1;
            let split_pos = Pos {
                line: tt.end.line,
                col: split_col,
            };
            let hat_tt = TokenTup {
                token: TokenContent::Real(Token::Op("^")),
                span: split_byte..tt.span.end,
                start: split_pos,
                end: tt.end,
            };
            let dotdot_tt = TokenTup {
                token: TokenContent::Real(Token::DotDot),
                span: tt.span.start..split_byte,
                start: tt.start,
                end: split_pos,
            };
            self.delay_token(hat_tt); //    pushed first → comes out second
            self.delay_token(dotdot_tt); // pushed second → comes out first
            return Step::Restart;
        }

        // Pre-dispatch HPA/HPB rules (FCS `rulesForBothSoftWhiteAndHardWhite`
        // at LexFilter.fs:2648-2656, called from `hwTokenFetch` at L1306
        // *before* any context handler runs). FCS-style:
        // `delayToken(HPA); delayToken(tokenTup);` puts IDENT back on top
        // with HPA underneath; the `continue;` restarts the loop, which
        // re-pops the IDENT (now followed by HPA, not LBRACK/LPAREN, so
        // this rule won't refire) and lets it flow through the normal
        // context-dispatch arms below — preserving e.g. CtxtVanilla pushes
        // and ModuleHead/attrs state updates.
        //
        // Why this can't sit at the bottom of the loop (where its sibling
        // typar trigger does): identifiers consumed by the ModuleHead
        // attrs arm (`module [<Foo(Name = "x")>] M`) early-return before
        // reaching the late dispatch, so the HPA virtual never gets
        // emitted at all. The typar trigger doesn't have an analogous bug
        // — no context arm early-returns on the typar-trigger token types
        // (DELEGATE / numeric / IDENT) at a position where an adjacent
        // `<` would parse as a typar list. (Codex review 2026-05.)
        if let TokenContent::Real(Token::Ident(_) | Token::QuotedIdent(_)) = &tt.token {
            if self.next_token_is_adjacent_lbrack(&tt) {
                let bracket = self
                    .peek_next_token_tup()
                    .expect("next_token_is_adjacent_lbrack succeeded ⇒ peek has bracket");
                let hpb = TokenTup {
                    token: TokenContent::Virtual(Virtual::HighPrecedenceBrackApp),
                    span: bracket.span.clone(),
                    start: bracket.start,
                    end: bracket.end,
                };
                self.delay_token(hpb);
                self.delay_token(tt);
                return Step::Restart;
            }
            if self.next_token_is_adjacent_lparen(&tt) {
                let paren = self
                    .peek_next_token_tup()
                    .expect("next_token_is_adjacent_lparen succeeded ⇒ peek has paren");
                let hpp = TokenTup {
                    token: TokenContent::Virtual(Virtual::HighPrecedenceParenApp),
                    span: paren.span.clone(),
                    start: paren.start,
                    end: paren.end,
                };
                self.delay_token(hpp);
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // EOF/IN/DONE/etc. force the head context closed.
        if self.token_forces_head_context_closure(&tt) {
            let ctxt = self.pop_ctxt().expect("guard ensures non-empty");
            match self.end_token_for_a_ctxt(&ctxt) {
                Some(virt) => {
                    return Step::Emit(self.insert_token(virt, tt));
                }
                None => {
                    // `reprocess()` — re-enter with the same token.
                    self.delay_token(tt);
                    return Step::Restart;
                }
            }
        }

        // FCS L1660-1671: `;;` with an empty offside stack resets the
        // filter — emit `;;` as a real token and re-arm `peek_initial`
        // so the next `next()` call pushes a fresh outer SeqBlock
        // anchored at the upcoming real token's position. FCS schedules
        // an `ORESET` synthetic for this; we collapse the two-step
        // dance into a single `initialized = false` flag flip because
        // our `next()` re-runs `peek_initial` whenever the flag clears.
        //
        // Reachable shape: a `;;` between top-level declarations runs
        // the per-arm `isSemiSemi ||` cascade through every layer of
        // the offside stack (RHS SeqBlock → LetDecl → body SeqBlock →
        // ModuleBody → outer SeqBlock(NoAddBlockEnd)). The outer
        // SeqBlock(NoAddBlockEnd) close reprocesses without emitting,
        // landing the same `;;` back here with an empty stack. Without
        // the re-seed, the following `let y` would arrive with no
        // context and surface as a raw `Let` instead of `OffsideLet`.
        if matches!(&tt.token, TokenContent::Real(Token::SemiSemi)) && self.offside_stack.is_empty()
        {
            self.initialized = false;
            self.last_real_end = tt.span.end;
            return Step::Emit(tt);
        }
        Step::Pass(tt)
    }
}
