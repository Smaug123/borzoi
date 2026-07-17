//! Interpolated-string and paren-balancing rules, plus the tail dispatch
//! (dummy drop, OBLOCKBEGIN passthrough, typar trigger, CtxtVanilla push).

use super::{
    AddBlockEnd, Context, Filter, Opener, Step, TokenContent, TokenTup, Virtual,
    is_typar_application_trigger,
};
use crate::lexer::{InterpKind, LexError, Span, Token};

impl<'a, I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>> Filter<'a, I> {
    /// Interpolated-string fill scopes, paren openers/closers, the dummy drop,
    /// the OBLOCKBEGIN passthrough, the typar-application trigger, and the
    /// catch-all CtxtVanilla push.
    pub(super) fn interp_and_paren(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // INTERP_STRING_BEGIN_PART (FCS LexFilter.fs:2280-2287, folded
        // into the same arm as TokenLExprParen/SIG). The fill body is a
        // fresh expression scope: push CtxtParen(InterpFill) *anchored at
        // the position after `$"…{`* (FCS `tokenTup.LexbufState.EndPos`,
        // not `tokenStartPos`) so the fill body's first token isn't
        // forced to align with the `$"…{` itself, then push CtxtSeqBlock
        // (NoAddBlockEnd — the fill body emits OBLOCKSEPs but no
        // surrounding OBLOCKBEGIN/OBLOCKEND), then increment paren_depth
        // and pass through. The bare-string `BeginEnd` / `TripleBeginEnd`
        // / `VerbatimBeginEnd` variants are atomic tokens (no body) and
        // not handled here. Triple-quoted (`TripleBegin`) and verbatim
        // (`VerbatimBegin`) opens push the same fill scope as
        // single-quoted opens — the offside rules and brace nesting
        // inside the fill are identical; only the surrounding string
        // delimiter differs. Extended opens (`$$"""…{{`, `ExtendedBegin`)
        // are likewise triple-like and push the same fill scope.
        // Multi-fill chains (`$"a={x}b={y}c"`) keep the PART/END
        // close-and-reopen dance in the closer arm below.
        if matches!(
            &tt.token,
            TokenContent::Real(Token::InterpString(
                InterpKind::Begin
                    | InterpKind::TripleBegin
                    | InterpKind::VerbatimBegin
                    | InterpKind::ExtendedBegin { .. }
            ))
        ) {
            self.push_ctxt(
                tt.span.clone(),
                Context::Paren {
                    pos: tt.end,
                    opener: Opener::InterpFill,
                },
            );
            self.push_ctxt_seq_block(&tt, AddBlockEnd::No);
            self.paren_depth += 1;
            self.last_real_end = tt.span.end;
            return Step::Emit(tt);
        }

        // INTERP_STRING_PART and INTERP_STRING_END (FCS LexFilter.fs:
        // 1697-1714). Both close the active fill. Inner contexts
        // (SeqBlock/Vanilla) were force-closed in
        // `token_forces_head_context_closure` above; CtxtParen(InterpFill)
        // is now at the head. For PART, immediately re-push a fresh
        // CtxtParen(InterpFill) + CtxtSeqBlock anchored at PART's EndPos
        // so the next fill's body gets its own scope. No ODUMMY is
        // queued at the closer (FCS L1709: `| INTERP_STRING_END _ -> ()`
        // and the PART branch does its own push instead).
        if let TokenContent::Real(Token::InterpString(
            kind @ (InterpKind::End { .. } | InterpKind::Part),
        )) = &tt.token
        {
            let kind = *kind;
            let balanced = self.token_balances_head_context(&tt, &self.offside_stack);
            if balanced {
                self.pop_ctxt();
                self.paren_depth = self.paren_depth.saturating_sub(1);
                if matches!(kind, InterpKind::Part) {
                    self.push_ctxt(
                        tt.span.clone(),
                        Context::Paren {
                            pos: tt.end,
                            opener: Opener::InterpFill,
                        },
                    );
                    self.push_ctxt_seq_block(&tt, AddBlockEnd::No);
                    self.paren_depth += 1;
                }
            }
            self.last_real_end = tt.span.end;
            return Step::Emit(tt);
        }

        // TokenLExprParen openers: push CtxtParen (so the matching
        // TokenRExprParen closer can force-close inner SeqBlock/CtxtFun
        // contexts before balancing), then push CtxtSeqBlock so aligned
        // statements inside brackets get OBLOCKSEP, then increment paren
        // depth and pass through. (LexFilter.fs:2281-2288)
        //
        // The inner SeqBlock is `NoAddBlockEnd` for most openers, but CLASS
        // (LexFilter.fs:2573) uses `AddBlockEnd` — the inner block emits an
        // OBLOCKBEGIN/OBLOCKEND pair around the class body. SIG
        // (LexFilter.fs:2281) is folded into the TokenLExprParen arm and
        // stays NoAddBlockEnd.
        let paren_opener = match &tt.token {
            TokenContent::Real(Token::LParen) => Some((Opener::Paren, AddBlockEnd::No)),
            TokenContent::Real(Token::LBrace) => Some((Opener::Brace, AddBlockEnd::No)),
            TokenContent::Real(Token::LBrack) => Some((Opener::Brack, AddBlockEnd::No)),
            TokenContent::Real(Token::LBrackBar) => Some((Opener::BrackBar, AddBlockEnd::No)),
            TokenContent::Real(Token::LBraceBar) => Some((Opener::BraceBar, AddBlockEnd::No)),
            TokenContent::Real(Token::Begin) => Some((Opener::Begin, AddBlockEnd::No)),
            TokenContent::Real(Token::Sig) => Some((Opener::Sig, AddBlockEnd::No)),
            TokenContent::Real(Token::Class) => Some((Opener::Class, AddBlockEnd::Yes)),
            TokenContent::Real(Token::LQuote) => Some((Opener::Quote, AddBlockEnd::No)),
            TokenContent::Real(Token::LQuoteRaw) => Some((Opener::QuoteRaw, AddBlockEnd::No)),
            // `<` after `peek_adjacent_typars` has promoted it to a typar
            // opener. Only `Less(true)` participates in offside paren
            // balancing — bare `Less(false)` (comparison) does not push
            // a CtxtParen and must not be balanced against `Greater(true)`.
            TokenContent::Real(Token::Less(true)) => Some((Opener::TyparAngle, AddBlockEnd::No)),
            _ => None,
        };
        if let Some((opener, add_block_end)) = paren_opener {
            self.push_ctxt(
                tt.span.clone(),
                Context::Paren {
                    pos: tt.start,
                    opener,
                },
            );
            self.push_ctxt_seq_block(&tt, add_block_end);
            self.paren_depth += 1;
            return Step::Emit(tt);
        }

        // TokenRExprParen closers: inner contexts (SeqBlock/CtxtFun) were
        // force-closed in `tokenForcesHeadContextClosure` above; CtxtParen
        // is now at the head. Pop it, decrement depth.
        // Swallowed by FCS's outer wrapper (→ drop): RPAREN, RBRACE.
        // Emitted unchanged (→ return): RBRACK, BAR_RBRACK, BAR_RBRACE,
        // RQUOTE, RQUOTERAW, END.
        //
        // Guard uses parenTokensBalance (LexFilter.fs:1699) so a mismatched
        // RQUOTE(raw=true) does not pop a CtxtParen opened by LQUOTE(raw=false)
        // and vice versa (and likewise for any other mismatched pair in
        // malformed input). (LexFilter.fs:2834, 1698)
        let closer_swallowed = match &tt.token {
            TokenContent::Real(Token::RParen) => Some(true),
            TokenContent::Real(Token::RBrace) => Some(true),
            TokenContent::Real(Token::RBrack) => Some(false),
            TokenContent::Real(Token::BarRBrack) => Some(false),
            TokenContent::Real(Token::BarRBrace) => Some(false),
            TokenContent::Real(Token::End) => Some(false),
            // RQUOTE is emitted by FCS (not swallowed by the outer wrapper).
            TokenContent::Real(Token::RQuote | Token::RQuoteRaw) => Some(false),
            // `>` after the typar scan has promoted it to a typar closer.
            // Emitted by FCS (`GREATER _` in TokenRExprParen): not swallowed.
            TokenContent::Real(Token::Greater(true)) => Some(false),
            _ => None,
        };
        if let Some(swallowed) = closer_swallowed {
            let balanced = self.token_balances_head_context(&tt, &self.offside_stack);
            if balanced {
                self.pop_ctxt();
                self.paren_depth = self.paren_depth.saturating_sub(1);
                // FCS queues an ODUMMY at the closer's position
                // (LexFilter.fs:1712) so any offside rule that applies
                // to the now-current head (in particular, the
                // `CtxtSeqBlock(NotFirstInSeqBlock)` OBLOCKSEP rule at
                // LexFilter.fs:1912) fires *between* the closer and
                // the next real token. Two motivating cases:
                //
                //   * Multi-line typar `let x =\n    Foo<\n        Bar\n
                //     >()`: `Greater(true)` lands at the outer
                //     SeqBlock's column on a different line, and FCS
                //     emits `Greater  OBLOCKSEP  HighPrecedenceParenApp`.
                //
                //   * STRUCT body under TypeDefns
                //     (`type T =\n    struct\n        val x: int\n    end`):
                //     END at the SeqBlock anchor column on a later line
                //     fires the OBLOCKSEP rule because END is *not* an
                //     `isTypeSeqBlockElementContinuator` (FCS L346 lists
                //     only BAR + virtual block/decl-ends), even though it
                //     *is* a general `isSeqBlockElementContinuator`
                //     (FCS L376). The asymmetry only matters under
                //     TypeDefns, so non-TypeDefns END balances stay inert.
                //
                // Other closers (`)`/`}`/`]`/…) are general
                // `isSeqBlockElementContinuator`s; under TypeDefns the
                // OBLOCKSEP rule could in theory fire for them too via
                // the same path, but those alignment shapes are rare in
                // practice.
                //
                // Second consumer: FCS's `relaxWhitespace2OffsideRule`
                // (LexFilter.fs:1473-1500) is a per-token predicate that
                // is `true` only when the current token is `ODUMMY` over
                // a `TokenRExprParen`. The sixteen offside-pop arms
                // (CtxtLetDecl/Match/Try/For/While/When/Exception/
                // InterfaceHead/If/Then/Else/WithAsLet/WithAsAugment/
                // TypeDefns/ModuleBody/NamespaceBody/MemberHead) gate
                // their `+1` bump on `relaxWhitespace2OffsideRule ||
                // isXContinuator`. To mirror FCS, queue a Dummy at every
                // `TokenRExprParen` closer; `is_relax_whitespace2_offside_rule`
                // unwraps the Dummy and triggers the bump.
                let queue_dummy = matches!(
                    &tt.token,
                    TokenContent::Real(
                        Token::RParen
                            | Token::RBrace
                            | Token::RBrack
                            | Token::BarRBrack
                            | Token::BarRBrace
                            | Token::End
                            | Token::RQuote
                            | Token::RQuoteRaw
                            | Token::Greater(true)
                    )
                );
                if queue_dummy && let TokenContent::Real(inner) = &tt.token {
                    let dummy = TokenTup {
                        token: TokenContent::Dummy {
                            prev_end: self.last_real_end,
                            inner: Box::new(inner.clone()),
                        },
                        span: tt.span.clone(),
                        start: tt.start,
                        end: tt.end,
                    };
                    self.delay_token(dummy);
                }
            }
            if swallowed {
                self.last_real_end = tt.span.end;
                return Step::Restart;
            } else {
                return Step::Emit(tt);
            }
        }

        // ODUMMY's pop-trigger rules (the offside-pop arms below)
        // would have fired already by now; reaching here means the
        // dummy did nothing, so drop it. (LexFilter.fs:2608)
        if let TokenContent::Dummy { .. } = &tt.token {
            return Step::Restart;
        }

        // OBLOCKBEGIN with the block rule disabled is a passthrough.
        // (LexFilter.fs:2600)
        if matches!(&tt.token, TokenContent::Virtual(Virtual::BlockBegin)) {
            return Step::Emit(tt);
        }

        // HPA/HPB ran as pre-dispatch above (mirror of FCS
        // `rulesForBothSoftWhiteAndHardWhite`); only the typar trigger
        // sits here. The two arms are FCS-siblings (LexFilter.fs:2650-2668)
        // but typars doesn't need the early-dispatch treatment — see the
        // comment on the HPA block above for why.
        //
        // Generic type-application disambiguation (FCS LexFilter.fs:2659-2668).
        // When an IDENT / DELEGATE / numeric literal is followed by an
        // adjacent `<`, run a paren-balance scan-ahead to decide whether
        // the `<` opens a typar list (`f<int>`, `list<string>`) or is
        // ordinary comparison (`a < b`). On success, inject
        // `HighPrecedenceTyApp` between `tt` and the rewritten
        // `Less(true)`; on failure the scan idempotently restores the
        // original stream and we fall through to the default passthrough.
        if is_typar_application_trigger(&tt.token) && self.peek_adjacent_typars(false, &tt) {
            // The candidate `<` is now atop `delayed` as `Less(_)`; FCS
            // re-pops and re-emits it as `Less(true)`, and inserts
            // `HighPrecedenceTyApp` at the `<`'s location. We do the
            // same — the inner smash pass deliberately leaves the
            // outer `Less` untouched so this site can canonicalise it.
            let less_tt = self
                .pop_next_token_tup()
                .expect("peek_adjacent_typars success ⇒ Less is on top of delayed");
            let less_span = less_tt.span.clone();
            let less_start = less_tt.start;
            let less_end = less_tt.end;
            self.delay_token(TokenTup {
                token: TokenContent::Real(Token::Less(true)),
                span: less_span.clone(),
                start: less_start,
                end: less_end,
            });
            self.delay_token(TokenTup {
                token: TokenContent::Virtual(Virtual::HighPrecedenceTyApp),
                span: less_span,
                start: less_start,
                end: less_end,
            });
            return Step::Emit(tt);
        }

        // Catch-all: an ordinary real token arriving with CtxtSeqBlock
        // on top pushes a CtxtVanilla anchored at the token's column,
        // then passes the token through. (LexFilter.fs:2617) Virtual
        // tokens (BlockBegin/BlockSep/etc.) don't push Vanilla.
        //
        // Critical for the RARROW push gate: the first real token of a
        // match-arm body lands on a SeqBlock(OneSided), pushes
        // CtxtVanilla, and that Vanilla blocks a subsequent `->` from
        // re-firing the RARROW push (Vanilla isn't in the gate's
        // allowed heads). Without this, the next arm's `->` would open
        // a duplicate OneSided SeqBlock and EOF would emit an extra
        // ORIGHT_BLOCK_END.
        if matches!(&tt.token, TokenContent::Real(_))
            && matches!(self.head(), Some(Context::SeqBlock { .. }))
        {
            let is_long_ident_equals = match &tt.token {
                TokenContent::Real(tok) => self.is_long_ident_equals(tok),
                _ => false,
            };
            self.push_ctxt(
                tt.span.clone(),
                Context::Vanilla {
                    pos: tt.start,
                    is_long_ident_equals,
                },
            );
            return Step::Emit(tt);
        }
        Step::Pass(tt)
    }
}
