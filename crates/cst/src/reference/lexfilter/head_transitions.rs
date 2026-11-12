//! CtxtNamespaceHead / CtxtModuleHead transition rules for `hw_token_fetch`.

use super::{
    AddBlockEnd, Context, Filter, ModuleHeadPrev, NamespacePrev, PushStrictness, Step,
    TokenContent, TokenTup, Virtual,
};
use crate::lexer::{LexError, Span, Token};

impl<'a, I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>> Filter<'a, I> {
    /// CtxtNamespaceHead / CtxtModuleHead dotted-ident transitions.
    pub(super) fn head_transitions(
        &mut self,
        tt: TokenTup<'a>,
        use_block_rule: &mut bool,
    ) -> Step<'a> {
        // CtxtNamespaceHead transition (LexFilter.fs:1726). While the
        // dotted-ident continues — and the continuation token sits to
        // the right of `namespace`'s column — accept it and update the
        // head's `prev` slot. The first non-continuation token pops
        // the head, pushes `CtxtNamespaceBody` anchored at `namespace`,
        // and pushes a SeqBlock(AddBlockEnd) anchored at the
        // next-token's column. Trailing EOF is handled by the
        // `token_forces_head_context_closure` cascade above; we never
        // arrive here with EOF on `tt`.
        if let Some(Context::NamespaceHead {
            pos: head_pos,
            prev,
        }) = self.head().cloned()
        {
            let is_kw_class = matches!(
                &tt.token,
                TokenContent::Real(
                    Token::Rec | Token::Global | Token::Ident(_) | Token::QuotedIdent(_)
                )
            );
            let is_dot = matches!(&tt.token, TokenContent::Real(Token::Dot));
            let head_col_lt = head_pos.col < tt.start.col;
            let accept_kw = prev == NamespacePrev::Keyword && is_kw_class && head_col_lt;
            let accept_dot = prev == NamespacePrev::Ident && is_dot && head_col_lt;
            if accept_kw || accept_dot {
                let new_prev = match &tt.token {
                    TokenContent::Real(Token::Ident(_) | Token::QuotedIdent(_)) => {
                        NamespacePrev::Ident
                    }
                    _ => NamespacePrev::Keyword,
                };
                self.pop_ctxt();
                self.push_ctxt(
                    tt.span.clone(),
                    Context::NamespaceHead {
                        pos: head_pos,
                        prev: new_prev,
                    },
                );
                return Step::Emit(tt);
            }
            // Transition out — fall through to body+SeqBlock.
            self.pop_ctxt();
            let fallback = tt.clone();
            self.delay_token(tt);
            self.push_ctxt(
                fallback.span.clone(),
                Context::NamespaceBody { pos: head_pos },
            );
            self.push_ctxt_seq_block_at(
                PushStrictness::AlwaysLenient,
                false,
                &fallback,
                AddBlockEnd::Yes,
            );
            *use_block_rule = false;
            return Step::Restart;
        }

        // CtxtModuleHead transition (LexFilter.fs:1747). The MODULE
        // keyword itself was swallowed at push time; this arm consumes
        // the head-state machine and exits either via EQUALS/COLON
        // (named-body shape: `module Foo = …`) or via a non-continuation
        // token (whole-file shape `module Foo\n…` when `rest` is just
        // `[CtxtSeqBlock]`, otherwise nested-module-statement shape
        // emitting OBLOCKSEP).
        //
        // `attrs` tracks the `[< … >]` block of post-`module`
        // attributes (`module [<X>] Foo`); inside attrs, arbitrary
        // tokens at col > head pass through unchanged and the
        // `GREATER_RBRACK` closer flips attrs back off.
        if let Some(Context::ModuleHead {
            pos: head_pos,
            prev,
            attrs,
            nested,
        }) = self.head().cloned()
        {
            let head_col_lt = head_pos.col < tt.start.col;
            let is_eq_or_colon =
                matches!(&tt.token, TokenContent::Real(Token::Equals | Token::Colon));

            // Inside `[<...>]` attributes: GREATER_RBRACK at col > head
            // closes the attrs block; any other token at col > head —
            // including `=` and `:`, which would otherwise be mistaken
            // for the module-body delimiter (e.g. `[<Foo(Name = "x")>]`,
            // `[<Foo(x : int)>]`) — passes through with no state change.
            // Must run before the EQUALS/COLON arm.
            //
            // RPAREN/RBRACE inside attrs are swallowed: FCS's outer
            // wrapper (LexFilter.fs:2834) unconditionally rewrites them
            // to *_COMING_SOON / *_IS_HERE faux tokens that the public
            // API filters as `FSharpTokenKind.None`. We don't model
            // those faux tokens — we just drop the closer, matching the
            // existing CtxtParen-balancing swallow at the catch-all.
            if attrs && head_col_lt {
                if matches!(&tt.token, TokenContent::Real(Token::GreaterRBrack)) {
                    self.pop_ctxt();
                    self.push_ctxt(
                        tt.span.clone(),
                        Context::ModuleHead {
                            pos: head_pos,
                            prev,
                            attrs: false,
                            nested,
                        },
                    );
                    return Step::Emit(tt);
                }
                if matches!(&tt.token, TokenContent::Real(Token::RParen | Token::RBrace)) {
                    self.last_real_end = tt.span.end;
                    return Step::Restart;
                }
                return Step::Emit(tt);
            }

            // EQUALS/COLON arm has NO column guard (LexFilter.fs:1771):
            // even a leftmost `=` transitions to ModuleBody+SeqBlock.
            if is_eq_or_colon {
                self.pop_ctxt();
                self.push_ctxt(
                    tt.span.clone(),
                    Context::ModuleBody {
                        pos: head_pos,
                        whole_file: false,
                    },
                );
                self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
                return Step::Emit(tt);
            }

            // Access modifiers (PUBLIC/PRIVATE/INTERNAL) after `module`:
            // pass through, no state change. Allowed only when
            // prev=Module — FCS L1761 requires `MODULE, (PUBLIC|...)`.
            if prev == ModuleHeadPrev::Module
                && head_col_lt
                && matches!(
                    &tt.token,
                    TokenContent::Real(Token::Public | Token::Private | Token::Internal)
                )
            {
                return Step::Emit(tt);
            }

            // `module [<...>]` attribute opener: flip attrs on, emit
            // the LBRACK_LESS. Only allowed when prev=Module.
            if prev == ModuleHeadPrev::Module
                && head_col_lt
                && matches!(&tt.token, TokenContent::Real(Token::LBrackLess))
            {
                self.pop_ctxt();
                self.push_ctxt(
                    tt.span.clone(),
                    Context::ModuleHead {
                        pos: head_pos,
                        prev,
                        attrs: true,
                        nested,
                    },
                );
                return Step::Emit(tt);
            }

            // The accept-set patterns (LexFilter.fs:1763):
            //   MODULE, GLOBAL                          → prev := GLOBAL
            //   (MODULE | REC | DOT), (REC | IDENT _)   → prev := token
            //   IDENT _, DOT                            → prev := DOT
            let accept_global = prev == ModuleHeadPrev::Module
                && matches!(&tt.token, TokenContent::Real(Token::Global))
                && head_col_lt;
            let accept_rec_or_ident =
                matches!(prev, ModuleHeadPrev::Module | ModuleHeadPrev::RecOrDot)
                    && matches!(
                        &tt.token,
                        TokenContent::Real(Token::Rec | Token::Ident(_) | Token::QuotedIdent(_))
                    )
                    && head_col_lt;
            let accept_dot = prev == ModuleHeadPrev::Ident
                && matches!(&tt.token, TokenContent::Real(Token::Dot))
                && head_col_lt;
            if accept_global || accept_rec_or_ident || accept_dot {
                let new_prev = match &tt.token {
                    TokenContent::Real(Token::Global) => ModuleHeadPrev::Global,
                    TokenContent::Real(Token::Ident(_) | Token::QuotedIdent(_)) => {
                        ModuleHeadPrev::Ident
                    }
                    // Rec / Dot collapse to the same accept state.
                    _ => ModuleHeadPrev::RecOrDot,
                };
                self.pop_ctxt();
                self.push_ctxt(
                    tt.span.clone(),
                    Context::ModuleHead {
                        pos: head_pos,
                        prev: new_prev,
                        attrs: false,
                        nested,
                    },
                );
                return Step::Emit(tt);
            }

            // Catch-all (LexFilter.fs:1777-1797): pop the head, then
            // either open a whole-file module body (when the only
            // remaining context is the top-level SeqBlock) or emit
            // OBLOCKSEP for nested-module-statement shape.
            self.pop_ctxt();
            let rest_is_top_seq =
                matches!(self.offside_stack.as_slice(), [Context::SeqBlock { .. }]);
            if rest_is_top_seq {
                let fallback = tt.clone();
                self.delay_token(tt);
                self.push_ctxt(
                    fallback.span.clone(),
                    Context::ModuleBody {
                        pos: head_pos,
                        whole_file: true,
                    },
                );
                self.push_ctxt_seq_block(&fallback, AddBlockEnd::Yes);
                *use_block_rule = false;
                return Step::Restart;
            }
            return Step::Emit(self.insert_token_from_prev_to_current(Virtual::BlockSep, tt));
        }
        Step::Pass(tt)
    }
}
