//! Offside-pop and balance rules for `hw_token_fetch` — the context-closing
//! half of the dispatch loop.

use super::{
    AddBlockEnd, Context, Filter, Opener, Step, TokenContent, TokenTup, Virtual, is_adjacent,
};
use crate::lexer::{LexError, Span, Token};

impl<'a, I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>> Filter<'a, I> {
    /// CtxtVanilla and CtxtSeqBlock offside machinery: the Vanilla/SeqBlock
    /// offside-pops, the first-in-block repull, and the OBLOCKSEP separator.
    pub(super) fn block_offside(
        &mut self,
        tt: TokenTup<'a>,
        use_block_rule: &mut bool,
    ) -> Step<'a> {
        // CtxtVanilla offside-pop: silent pop + reprocess when the
        // current token starts at or left of the Vanilla anchor.
        // (LexFilter.fs:1868) Vanilla is the catch-all ordinary-token
        // context that sits on top of a CtxtSeqBlock; once the
        // expression it covered ends (offside or EOF cascade), drop it
        // and re-examine the token against the underlying SeqBlock.
        // `isSemiSemi` short-circuits indentation entirely: `;;` always
        // closes Vanilla so the cascade reaches the SeqBlock below.
        if let Some(Context::Vanilla { pos, .. }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi || tt.start.col <= pos.col {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // SeqBlock offside-pop: token starts strictly to the left of the
        // SeqBlock's anchor → close the block. `AddBlockEnd::Yes` emits a
        // virtual `BlockEnd` (filtered by the harness); `No` just
        // reprocesses. (LexFilter.fs:1803)
        //
        // Three grace adjustments from FCS L1807-1854 are ported here:
        //
        //   * BAR + CtxtTypeDefns beneath → grace=2 (L1813). Permits
        //     leading-bar DU arms slightly to the left of the inner
        //     SeqBlock anchor without prematurely closing it.
        //   * CtxtTypeDefns beneath, token's col equals the TypeDefns
        //     anchor, and token is *not* a type-seq-block-element
        //     continuator → grace=-1 (L1823). Closes the inner SeqBlock
        //     when a non-`|` declaration (`and`, `member`, an
        //     unrelated `let`) appears aligned with the `type` keyword,
        //     so the surrounding offside rules can run instead.
        //   * NAMESPACE + CtxtNamespaceBody beneath, namespace-body
        //     SeqBlock aligned with the namespace anchor → grace=-1
        //     (L1831). Pops the body's inner SeqBlock when a new
        //     `namespace` declaration arrives at the same column as
        //     the namespace anchor; the body's NamespaceBody then
        //     pops via its own offside-pop and the outer SeqBlock's
        //     OBLOCKSEP fires.
        //
        // `isSemiSemi` (LexFilter.fs:1806) short-circuits the column
        // check entirely: `;;` forces the SeqBlock closed regardless
        // of indentation — UNLESS the next context underneath is a
        // namespace body or a whole-file module body, in which case
        // `;;` is preserved as a real top-level token. Skipped: the
        // infix `grace` adjustment.
        if let Some(Context::SeqBlock {
            pos, add_block_end, ..
        }) = self.head().cloned()
        {
            let rest = if self.offside_stack.len() >= 2 {
                self.offside_stack.get(self.offside_stack.len() - 2)
            } else {
                None
            };
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi))
                && !matches!(
                    rest,
                    Some(Context::NamespaceBody { .. })
                        | Some(Context::ModuleBody {
                            whole_file: true,
                            ..
                        })
                );
            let grace: i32 = match (&tt.token, rest) {
                (TokenContent::Real(Token::Bar), Some(Context::TypeDefns { .. })) => 2,
                (_, Some(Context::TypeDefns { pos: type_pos, .. }))
                    if pos.col == type_pos.col
                        && !Self::is_type_seq_block_element_continuator(&tt.token) =>
                {
                    -1
                }
                (
                    TokenContent::Real(Token::Namespace),
                    Some(Context::NamespaceBody { pos: ns_pos }),
                ) if pos.col == ns_pos.col => -1,
                // Infix grace (LexFilter.fs:1833-1854): a leading infix operator
                // may sit up to `infixTokenLength + 1` columns left of the
                // SeqBlock anchor without closing it — the ubiquitous undented
                // `|> f` / `+ expr` continuation:
                //     let x =
                //           expr
                //        |> f expr    <-- `|>` left of `expr`, still a continuation
                //
                // FCS's grace classifies on the *raw* infix token (`isInfix`),
                // which fires *before* the `ADJACENT_PREFIX_OP` rewrite. So a
                // prefix-capable op glued to its operand (`-x`) keeps the grace
                // and the block stays open too — only its emitted *kind* differs
                // (FCS: `ADJACENT_PREFIX_OP`). Do NOT exclude glued prefix ops
                // here: doing so inserts a `BlockEnd` FCS never emits. (The
                // adjacency check belongs only to the aligned OBLOCKSEP path
                // below, where a glued prefix op *starts* a new statement.)
                _ if Self::token_is_infix(&tt.token) => {
                    Self::infix_token_length(&tt.token) as i32 + 1
                }
                _ => 0,
            };
            let lhs = (tt.start.col as i32) + grace;
            let rhs = pos.col as i32;
            if is_semi_semi || lhs < rhs {
                self.pop_ctxt();
                match add_block_end {
                    AddBlockEnd::Yes => {
                        return Step::Emit(self.insert_token(Virtual::BlockEnd, tt));
                    }
                    AddBlockEnd::OneSided => {
                        return Step::Emit(self.insert_token(Virtual::RightBlockEnd, tt));
                    }
                    AddBlockEnd::No => {
                        self.delay_token(tt);
                        return Step::Restart;
                    }
                }
            }
        }

        // SeqBlock(first=true) repull: flip to first=false and reprocess
        // without the block rule, so per-token offside checks see a
        // stable head. (LexFilter.fs:1884)
        if *use_block_rule
            && let Some(Context::SeqBlock {
                first: true,
                pos,
                add_block_end,
            }) = self.head().cloned()
        {
            self.pop_ctxt();
            self.push_ctxt(
                tt.span.clone(),
                Context::SeqBlock {
                    first: false,
                    pos,
                    add_block_end,
                },
            );
            self.delay_token(tt);
            *use_block_rule = false;
            return Step::Restart;
        }

        // SeqBlock(NotFirst) OBLOCKSEP: same column on a different line
        // means a new statement in the same block. Flip back to first=true
        // so the subsequent repull is a clean entry, and emit OBLOCKSEP
        // spanning prev-end → current-start. (LexFilter.fs:1912)
        //
        // Suppression branches by what sits beneath the SeqBlock
        // (LexFilter.fs:1914-1919):
        //   * CtxtNamespaceBody → suppress only when token is NAMESPACE
        //     (i.e. emit OBLOCKSEP for any other aligned token; the
        //     NamespaceBody offside-pop above handles the NAMESPACE case
        //     by popping the body first).
        //   * CtxtTypeDefns → suppress when token is an
        //     `isTypeSeqBlockElementContinuator` (i.e. `|` or virtual
        //     block-end / decl-end). The grace=-1 arm above already
        //     popped the inner SeqBlock for non-continuator aligned
        //     tokens, so the OBLOCKSEP arm only fires here when token
        //     IS a continuator and should be suppressed.
        //   * Otherwise → suppress for `isSeqBlockElementContinuator`
        //     (infix operators, closing brackets, `then`/`else`/...).
        if *use_block_rule
            && let Some(Context::SeqBlock {
                first: false,
                pos,
                add_block_end,
            }) = self.head().cloned()
            && tt.start.col == pos.col
            && tt.start.line != pos.line
        {
            // A prefix-capable op (`+ - +. -. & &&`) in *prefix position* is
            // FCS's `ADJACENT_PREFIX_OP` — a term-starter outside `isInfix`, so it
            // begins a new SeqBlock statement (`g x⏎ -1`) rather than continuing
            // the prior one (`g x (-1)`). FCS's prefix-position test is: glued to
            // the *next* token AND not glued-left to a preceding atomic-expression
            // end (`prevWasAtomicEnd && lastTokenPos = startPos` → infix). So a
            // spaced `- 1` (not glued-right) and a glued-left `x-1` after an atomic
            // end both stay the infix continuation. Our lexer emits one token
            // regardless of spacing (sign-folding is later), so reconstruct both
            // adjacency checks here; the peek caches the next token, and the borrow
            // must end before `rest` borrows the stack.
            let glued_left_to_atomic =
                self.last_real_was_atomic_end && self.last_real_end == tt.span.start;
            let adjacent_prefix_op = !glued_left_to_atomic
                && Self::is_adjacent_prefix_capable_op(&tt.token)
                && self
                    .peek_next_token_tup()
                    .is_some_and(|next| is_adjacent(&tt, &next));
            let rest = if self.offside_stack.len() >= 2 {
                self.offside_stack.get(self.offside_stack.len() - 2)
            } else {
                None
            };
            let suppress = match rest {
                Some(Context::NamespaceBody { .. }) => {
                    matches!(&tt.token, TokenContent::Real(Token::Namespace))
                }
                Some(Context::TypeDefns { .. }) => {
                    Self::is_type_seq_block_element_continuator(&tt.token)
                }
                // The adjacent-prefix op is a term-starter, never a continuator.
                _ => !adjacent_prefix_op && Self::is_seq_block_element_continuator(&tt.token),
            };
            if !suppress {
                self.pop_ctxt();
                self.push_ctxt(
                    tt.span.clone(),
                    Context::SeqBlock {
                        first: true,
                        pos,
                        add_block_end,
                    },
                );
                return Step::Emit(self.insert_token_from_prev_to_current(Virtual::BlockSep, tt));
            }
        }
        Step::Pass(tt)
    }

    /// FCS's `detectJoinInCtxt` (LexFilter.fs:747) — `true` when an `in`
    /// token should be rewritten to `JOIN_IN` (the query computation-expression
    /// join operator) rather than treated as a `let … in` / `for … in`
    /// keyword. The condition is purely structural: the head context is a
    /// `CtxtVanilla` and the first context beneath it that is *not* a
    /// seq-block / `do` / `for` scope is a brace `CtxtParen({)` — i.e. we're
    /// directly inside a `{ … }` computation-expression body. It is *not* tied
    /// to the `join`/`on` words, so `query { a in b }` is also a `JoinIn`.
    ///
    /// The offside stack is stored top-last (`head()` is `last()`), the mirror
    /// of FCS's head-first list, so the walk runs from the back: the top must
    /// be `Vanilla`, then scanning toward the front the first non-skipped
    /// context decides (brace ⇒ `true`, anything else ⇒ `false`). FCS matches
    /// `LBRACE` only, so the bare `{` (`Opener::Brace`) qualifies but `{|`
    /// (`Opener::BraceBar`) does not.
    pub(super) fn detect_join_in_ctxt(stack: &[Context]) -> bool {
        let Some((Context::Vanilla { .. }, rest)) = stack.split_last() else {
            return false;
        };
        for ctxt in rest.iter().rev() {
            match ctxt {
                Context::SeqBlock { .. } | Context::Do { .. } | Context::For { .. } => continue,
                Context::Paren {
                    opener: Opener::Brace,
                    ..
                } => return true,
                _ => return false,
            }
        }
        false
    }

    /// IN → JOIN_IN rewrite: inside a query computation-expression brace
    /// (`detect_join_in_ctxt`), the `in` is the join operator, not a
    /// `let … in` / `for … in` keyword. Rewrite it to a backed-by-raw
    /// `Virtual::JoinIn` (the raw `Token::In` stays in the stream at the same
    /// span, so the parser emits an `IN_TOK` for it) and record its end so any
    /// following virtual's span starts past the `in`.
    ///
    /// Runs as its own pipeline step *before* `block_offside`, mirroring FCS,
    /// where the `IN, detectJoinInCtxt -> JOIN_IN` arm (LexFilter.fs:1674)
    /// precedes the `CtxtVanilla` / `CtxtSeqBlock` offside pops
    /// (LexFilter.fs:1868). A join `in` on its own line at/left of the
    /// preceding clause's `Vanilla` anchor (`query {⏎  join x⏎  in xs⏎}`) would
    /// otherwise have its `Vanilla` popped and an `OBLOCKSEP` inserted by
    /// `block_offside` first — splitting the join into two statements and
    /// emitting a raw `In`. The force-closure in `predispatch` runs earlier
    /// still, but the join `in` *balances* its head (`token_balances_head_context`)
    /// so it is never force-closed. (LexFilter.fs:1674.)
    pub(super) fn join_in_rewrite(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        if let TokenContent::Real(Token::In) = &tt.token
            && Self::detect_join_in_ctxt(&self.offside_stack)
        {
            self.last_real_end = tt.span.end;
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::JoinIn),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }
        Step::Pass(tt)
    }

    /// IN-balances-CtxtLetDecl and DONE-balances-CtxtDo arms.
    pub(super) fn in_done_balances(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // IN balances with CtxtLetDecl: pop the LetDecl, queue an ODUMMY
        // at IN's position so further pop rules can fire, and replace IN
        // with ODECLEND (block-let) or pass IN through (non-block-let).
        // (LexFilter.fs:1679)
        //
        // Must come before the CtxtLetDecl offside-pop below: when `in`
        // sits at or left of the `let`'s column (e.g. `let x = 1\nin x`),
        // both rules' guards hold but FCS's `in` arm runs first and
        // swallows the `in`, replacing it with ODECLEND. The indentation
        // warning (`tokenStartCol < offsidePos`) is elided — diagnostics
        // aren't wired yet; first input that misindents `in` will force
        // that port.
        if let TokenContent::Real(Token::In) = &tt.token
            && let Some(Context::LetDecl { block_let, .. }) = self.head().cloned()
        {
            self.pop_ctxt();
            let dummy = TokenTup {
                token: TokenContent::Dummy {
                    prev_end: self.last_real_end,
                    inner: Box::new(Token::In),
                },
                span: tt.span.clone(),
                start: tt.start,
                end: tt.end,
            };
            self.delay_token(dummy);
            if block_let {
                // `in` is a real token being consumed: record its end so
                // insert_token_from_prev_to_current can compute correct spans
                // for any following virtual tokens (e.g. OBLOCKSEP).
                self.last_real_end = tt.span.end;
                return Step::Emit(TokenTup {
                    token: TokenContent::Virtual(Virtual::DeclEnd),
                    span: tt.span,
                    start: tt.start,
                    end: tt.end,
                });
            }
            return Step::Emit(tt);
        }

        // DONE balances CtxtDo (LexFilter.fs:1689) — even a non-offside
        // `done` closes its `do`. Replace DONE with `OffsideDeclEnd` at
        // DONE's own range, then continue so the synthesised DeclEnd
        // goes through subsequent offside-pop arms. The DONE token
        // itself is swallowed; advance `last_real_end` past it so any
        // following OBLOCKSEP span starts after `done`. The inner
        // `CtxtSeqBlock(AddBlockEnd)` is already off the stack by the
        // time we get here — `tokenForcesHeadContextClosure` pops it
        // first (emitting an internal OBLOCKEND).
        if let TokenContent::Real(Token::Done) = &tt.token
            && let Some(Context::Do { .. }) = self.head()
        {
            self.pop_ctxt();
            self.last_real_end = tt.span.end;
            let decl_end = TokenTup {
                token: TokenContent::Virtual(Virtual::DeclEnd),
                span: tt.span.clone(),
                start: tt.start,
                end: tt.end,
            };
            self.delay_token(decl_end);
            return Step::Restart;
        }
        Step::Pass(tt)
    }

    /// Offside-pops for the keyword-introduced expression contexts
    /// (match/try/for/do/while/when/fun/function/exception/interface/if/then/else).
    pub(super) fn keyword_offside_pops(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // CtxtMatch offside-pop: token at column ≤ `match`'s column
        // closes the match-scrutinee scope silently
        // (`endTokenForACtxt` = None) and reprocesses. `WITH` is the
        // only real continuator that keeps it open (via
        // `isMatchBlockContinuator`, LexFilter.fs:223).
        // `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500) also
        // bumps the guard for `ODUMMY TokenRExprParen`. `isSemiSemi`
        // short-circuits indentation entirely. (LexFilter.fs:2031)
        if let Some(Context::Match { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_match_block_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtTry offside-pop: token at column ≤ `try`'s column closes
        // the try scope silently (`endTokenForACtxt` = None) and
        // reprocesses. `WITH` and `FINALLY` are the continuators that
        // keep it open until the balance arm fires (LexFilter.fs:236).
        // `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500) also
        // bumps the guard for `ODUMMY TokenRExprParen`. `isSemiSemi`
        // short-circuits indentation entirely. (LexFilter.fs:2073)
        if let Some(Context::Try { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_try_block_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtFor offside-pop: token at column ≤ `for`'s column closes
        // the for-loop scope. `isForLoopContinuator` (LexFilter.fs:314)
        // bumps the guard to `tokenStartCol + 1 <= offsidePos` so that
        // an aligned `done` (or the reprocessed virtual `ODECLEND` it
        // delays) keeps the for-loop open until the trailing `in`
        // balances it. `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500)
        // also bumps the guard for `ODUMMY TokenRExprParen`. `isSemiSemi`
        // short-circuits indentation entirely. (LexFilter.fs:2037)
        if let Some(Context::For { pos, .. }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_for_loop_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtDo offside-pop: token at column ≤ `do`'s column closes the
        // do-clause, emitting `ODECLEND` per `endTokenForACtxt`.
        // `isDoContinuator` (LexFilter.fs:254) bumps the guard to `+1 <=`
        // so an inner `done`'s reprocessed `Virtual::DeclEnd` landing at
        // exactly the outer `do`'s column keeps it open until the outer
        // `do`'s own `done` (or EOF) arrives. FCS does NOT apply the
        // `relaxWhitespace2OffsideRule` bump here. `isSemiSemi` still
        // short-circuits indentation entirely. (LexFilter.fs:1949)
        if let Some(Context::Do { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col + u32::from(Self::is_do_continuator(&tt.token)) <= pos.col
            {
                self.pop_ctxt();
                return Step::Emit(self.insert_token(Virtual::DeclEnd, tt));
            }
        }

        // CtxtWhile offside-pop: token at column ≤ `while`'s column closes
        // the while-scope silently (`endTokenForACtxt` = None). `DONE`
        // is the only non-virtual continuator so far; aligned `done` keeps
        // the scope open via the +1 guard. `relaxWhitespace2OffsideRule`
        // (LexFilter.fs:1473-1500) also bumps the guard for
        // `ODUMMY TokenRExprParen`. `isSemiSemi` short-circuits
        // indentation entirely. (LexFilter.fs:2043)
        if let Some(Context::While { pos, .. }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_while_block_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtWhen offside-pop: token at column ≤ `when`'s column closes
        // the guard scope silently. No continuator predicate in FCS —
        // `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500) is the
        // only guard-bump arm. `isSemiSemi` short-circuits indentation
        // entirely. (LexFilter.fs:2049-2053)
        if let Some(Context::When { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col + u32::from(Self::is_relax_whitespace2_offside_rule(&tt.token))
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtFun offside-pop: token at column ≤ `fun`'s column closes
        // the lambda scope, emitting OEND. No continuator predicate —
        // `relaxWhitespace2OffsideRule` is hard-disabled for CtxtFun (the
        // `(*relaxWhitespace2OffsideRule*)false` guard at LexFilter.fs:
        // 2059-2065). `isSemiSemi` still short-circuits indentation
        // entirely (FCS keeps `isSemiSemi || …` on this arm).
        // (LexFilter.fs:2055)
        if let Some(Context::Fun { pos, .. }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi || tt.start.col <= pos.col {
                self.pop_ctxt();
                return Step::Emit(self.insert_token(Virtual::End, tt));
            }
        }

        // CtxtFunction offside-pop: token at column ≤ `function`'s
        // column closes the scope silently and reprocesses. No virtual
        // token — endTokenForACtxt is `_ -> None` for CtxtFunction
        // (LexFilter.fs:1545). The companion CtxtMatchClauses
        // underneath supplies the OEND when its own offside arm fires.
        // `isSemiSemi` short-circuits indentation entirely
        // (FCS keeps `isSemiSemi || …` on this arm).
        // (LexFilter.fs:2068-2071)
        if let Some(Context::Function { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi || tt.start.col <= pos.col {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtException offside-pop: token at column ≤ `exception`'s
        // column (or any `;;`) closes the scope silently and reprocesses.
        // No virtual emitted (endTokenForACtxt returns None at L1545).
        // `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500) bumps
        // the guard for `ODUMMY TokenRExprParen`. (LexFilter.fs:1990)
        if let Some(Context::Exception { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col + u32::from(Self::is_relax_whitespace2_offside_rule(&tt.token))
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtInterfaceHead offside-pop (LexFilter.fs:1960-1962): silent
        // pop on `;;` or token column ≤ anchor column. `isInterfaceContinuator`
        // (END / reprocessed-virtual-endings) lets aligned `end` (or a
        // cascading OBLOCKEND / ODECLEND landing at the head's column)
        // keep the head open: with `col + 1 <= anchor.col`, a sibling at
        // `anchor.col` still closes (1 + col > anchor.col only when col >=
        // anchor.col, so the guard kicks in iff col < anchor.col), letting
        // an explicit `end` reach the inner WithAsAugment first.
        // `endTokenForACtxt` returns None, so the pop emits no virtual.
        if let Some(Context::InterfaceHead { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            let threshold_col = tt.start.col
                + u32::from(
                    Self::is_relax_whitespace2_offside_rule(&tt.token)
                        || Self::is_interface_continuator(&tt.token),
                );
            if is_semi_semi || threshold_col <= pos.col {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtIf / CtxtThen / CtxtElse offside-pops: token at column ≤
        // anchor closes the context. All three pop silently (no virtual
        // token — `endTokenForACtxt` returns None for them) and
        // reprocess. (LexFilter.fs:2013, 2085, 2093)
        //
        // CtxtIf's guard is gated by `isIfBlockContinuator` so aligned
        // THEN/ELSE/ELIF keep the conditional open. Without it,
        // `let f c =\n    if c\n    then 1\n    else 2` pops CtxtIf the
        // moment `then` aligns with `if`, dropping the THEN-body
        // SeqBlock's force-closure path (because ELSE can no longer
        // find a balancing CtxtIf via `suffixExists`).
        //
        // CtxtThen / CtxtElse skip their continuator predicates
        // (`isThenBlockContinuator` is just the reprocessed-virtual
        // tokens; CtxtElse has no continuator). Aligned bodies under
        // `then`/`else` aren't reached by current tests; the EOF
        // cascade pops CtxtThen/CtxtElse via `tokenForcesHeadContextClosure`.
        if let Some(Context::If { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_if_block_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }
        if let Some(Context::Then { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_then_block_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }
        if let Some(Context::Else { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col + u32::from(Self::is_relax_whitespace2_offside_rule(&tt.token))
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }
        Step::Pass(tt)
    }

    /// Offside-pops for CtxtMatchClauses, CtxtLetDecl, CtxtWithAsLet, CtxtWithAsAugment.
    pub(super) fn clause_offside_pops(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // CtxtMatchClauses offside-pop. Three sub-cases, all keyed on
        // strict `<` (not `<=`!): the guard column is shifted by ±1
        // depending on `leadingBar` and the incoming token (LexFilter.fs:
        // 2099-2113):
        //   BAR with leadingBar=true  : col + 0 < anchor
        //   BAR with leadingBar=false : col + 2 < anchor
        //   END with leadingBar=true  : col - 1 < anchor (i.e. col < anchor + 1)
        //   END with leadingBar=false : col + 1 < anchor
        //   other with leadingBar=true: col - 1 < anchor
        //   other with leadingBar=false: col + 1 < anchor
        // Closes with OEND (`endTokenForACtxt` returns Some(End) for
        // CtxtMatchClauses, LexFilter.fs:1526).
        if let Some(Context::MatchClauses { leading_bar, pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            // Compute `tokenStartCol + shift` per FCS. Use i64 because
            // the shift can be -1 (leadingBar=true, END/other).
            let shift: i64 = match (&tt.token, leading_bar) {
                (TokenContent::Real(Token::Bar), true) => 0,
                (TokenContent::Real(Token::Bar), false) => 2,
                (_, true) => -1,
                (_, false) => 1,
            };
            let shifted = (tt.start.col as i64) + shift;
            if is_semi_semi || shifted < pos.col as i64 {
                self.pop_ctxt();
                return Step::Emit(self.insert_token(Virtual::End, tt));
            }
        }

        // CtxtLetDecl offside-pop: token at column ≤ `let`'s column
        // closes the binding's declaration scope. Emits `ODECLEND` for
        // block-lets. (LexFilter.fs:1939)
        //
        // `isLetContinuator` (LexFilter.fs:336) — currently only `AND` —
        // bumps the guard to `tokenStartCol + 1 <= offsidePos`, so an
        // `and` aligned with `let` keeps the LetDecl open for the next
        // RHS push. `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500)
        // applies the same `+1` bump when the current token is an
        // `ODUMMY TokenRExprParen` — so a paren-closer dedented past the
        // `let` anchor pops the LetDecl at the closer's range rather than
        // at the next real token. `isSemiSemi` short-circuits indentation
        // entirely *for block lets only* — FCS L1939 matches
        // `CtxtLetDecl (true, _) :: _`, so a non-block let (the
        // `let x = 1 in x` shape) keeps the binding open through `;;`
        // and only pops via the dedicated IN arm or force-closure.
        if let Some(Context::LetDecl { block_let, pos }) = self.head().cloned() {
            let is_semi_semi =
                block_let && matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_let_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                if block_let {
                    return Step::Emit(self.insert_token(Virtual::DeclEnd, tt));
                }
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtWithAsLet offside-pop: token starts at or left of the
        // WithAsLet's anchor → pop and emit OEND. (LexFilter.fs:2019)
        // Reuses `isLetContinuator` (same +1 guard as the LetDecl arm
        // above) since `and` aligned with a record-update binding head
        // would be a continuation, not a close. The OEND is what the
        // parser needs to terminate the binding sequence even when the
        // closing `}` is missing — the force-closure path on `}` covers
        // the well-formed case via `endTokenForACtxt = Some(OEND)`.
        //
        // `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500) also
        // bumps the guard for `ODUMMY TokenRExprParen`. `isSemiSemi`
        // short-circuits indentation entirely.
        if let Some(Context::WithAsLet { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_let_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                return Step::Emit(self.insert_token(Virtual::End, tt));
            }
        }

        // CtxtWithAsAugment offside-pop: token starts at or left of the
        // augmentation's anchor → pop and emit ODECLEND.
        // (LexFilter.fs:2025-2029)
        //
        // `isWithAugmentBlockContinuator` is END only (L383-392) — an
        // `end` aligned with the `with` keeps the construct open and is
        // handled by the dedicated balance arm (L1717-1722) instead.
        // `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500) also
        // bumps the guard for `ODUMMY TokenRExprParen` — and in
        // particular for the `ODUMMY END` queued by the dedicated
        // augment-close arm below, so an aligned `end` followed by a
        // continuation reprocessed via the Dummy doesn't repop the
        // outer WithAsAugment here. `isSemiSemi` short-circuits
        // indentation entirely.
        if let Some(Context::WithAsAugment { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_with_augment_block_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                return Step::Emit(self.insert_token(Virtual::DeclEnd, tt));
            }
        }
        Step::Pass(tt)
    }

    /// END-balances-CtxtWithAsAugment arm.
    pub(super) fn end_balances_augment(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // END + CtxtWithAsAugment balance arm (LexFilter.fs:1717-1722).
        // Fires when END is *not* offside (tokenStartCol >= pos.col) so
        // the offside-pop arm above has refused. Per FCS L1262 END
        // *balances* WithAsAugment, so `tokenForcesHeadContextClosure`
        // short-circuits and never enters this code path — the only way
        // here is via direct dispatch on `End`. Pop WithAsAugment, queue
        // a Dummy at END's range (FCS's `ODUMMY END` — our Dummy carries
        // no inner token, so subsequent offside-pop arms can't recognise
        // END-specific continuator bumps for the queued marker), and
        // emit OEND with END's range. The original END is consumed (we
        // do not delay tt itself; only the synthetic Dummy gets re-fed).
        if let TokenContent::Real(Token::End) = &tt.token
            && let Some(Context::WithAsAugment { pos }) = self.head().cloned()
            && tt.start.col + 1 > pos.col
        {
            self.pop_ctxt();
            let dummy = TokenTup {
                token: TokenContent::Dummy {
                    prev_end: self.last_real_end,
                    inner: Box::new(Token::End),
                },
                span: tt.span.clone(),
                start: tt.start,
                end: tt.end,
            };
            self.delay_token(dummy);
            self.last_real_end = tt.span.end;
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::End),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }
        Step::Pass(tt)
    }

    /// Offside-pops for the declaration-body contexts (type/module/namespace/member).
    pub(super) fn decl_offside_pops(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // CtxtTypeDefns offside-pop (LexFilter.fs:1966-1970). Guard
        // mirrors `if isTypeContinuator token then col+1 else col` —
        // `AND` / `BAR` / `WITH` / `END` / `}` aligned with `type`
        // keep the construct open via the `+1` bump (see
        // `is_type_continuator` above). `relaxWhitespace2OffsideRule`
        // (LexFilter.fs:1473-1500) also bumps for `ODUMMY TokenRExprParen`.
        // `isSemiSemi` short-circuits indentation entirely.
        if let Some(Context::TypeDefns { pos, .. }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col
                    + u32::from(
                        Self::is_relax_whitespace2_offside_rule(&tt.token)
                            || Self::is_type_continuator(&tt.token),
                    )
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtModuleBody offside-pop (LexFilter.fs:1979): silent pop +
        // reprocess when the current token starts at or left of the
        // module anchor. `relaxWhitespace2OffsideRule` (LexFilter.fs:
        // 1473-1500) bumps the guard for `ODUMMY TokenRExprParen`.
        // `isSemiSemi` short-circuits indentation entirely *only when*
        // the body is not whole-file: a `module A` (no `=` / `:`) form
        // wraps the entire file and must NOT be torn down by `;;`.
        // For whole-file modules anchored at col 0 with same-col body
        // tokens, the inner SeqBlock at col 0 keeps the column-based
        // guard dormant until EOF — by which point
        // `token_forces_head_context_closure` handles cleanup.
        if let Some(Context::ModuleBody { pos, whole_file }) = self.head().cloned() {
            let is_semi_semi =
                !whole_file && matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col + u32::from(Self::is_relax_whitespace2_offside_rule(&tt.token))
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtNamespaceBody offside-pop (LexFilter.fs:1985). Guard
        // mirrors `if isNamespaceContinuator token then col+1 else col`:
        // every token except EOF and NAMESPACE is a continuator and so
        // needs `col + 1 <= offsidePos.col`. For a body anchored at
        // col 0 the continuator branch is unreachable (`col + 1 <= 0`
        // never holds), so the body is only popped by a NAMESPACE
        // arriving at col ≤ anchor — matching FCS's intent that only
        // a new `namespace` declaration terminates the prior namespace.
        // `relaxWhitespace2OffsideRule` (LexFilter.fs:1473-1500) is
        // already subsumed because `ODUMMY TokenRExprParen` is *not*
        // NAMESPACE, hence already a continuator getting the `+1` bump
        // — explicit OR is a no-op but kept for legibility.
        // EOF is handled by `token_forces_head_context_closure`.
        if let Some(Context::NamespaceBody { pos }) = self.head().cloned() {
            let is_continuator = !matches!(&tt.token, TokenContent::Real(Token::Namespace));
            let bump =
                u32::from(is_continuator || Self::is_relax_whitespace2_offside_rule(&tt.token));
            if tt.start.col + bump <= pos.col {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }

        // CtxtMemberBody offside-pop (LexFilter.fs:2002-2005). Token at
        // column ≤ the member-head anchor closes the body and emits
        // `ODECLEND`. FCS hard-codes the `relaxWhitespace2OffsideRule`
        // bump OFF here (the `if false then …` guard) for backcompat:
        //     member _.f() = [
        //         1 // intentionally aligned with the member, not offside
        //     ]
        // So unlike most offside arms, no `+1` continuator bump applies.
        //
        // `isSemiSemi` short-circuits indentation entirely, BUT the pop
        // emits NO virtual token — FCS's force-closure path
        // (L1556 + L1545) intercepts `;;` before this arm and uses
        // `endTokenForACtxt`, which is `None` for `CtxtMemberBody`.
        // The Rust port doesn't route `;;` through force-closure, so
        // we emulate the silent pop here: on `;;` the body closes via
        // `delay + continue`, never via `ODECLEND`. Without this
        // suppression, `member x.Data = arr;;` (an FSI-style member
        // ending in `;;`) would emit a stray `OffsideDeclEnd` before
        // the `SemicolonSemicolon` real token.
        if let Some(Context::MemberBody { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
            if tt.start.col <= pos.col {
                self.pop_ctxt();
                return Step::Emit(self.insert_token(Virtual::DeclEnd, tt));
            }
        }

        // CtxtMemberHead offside-pop (LexFilter.fs:2007-2011). Silent
        // pop + reprocess when the head's prelude (everything before
        // `=`) wraps onto a less-indented line. `relaxWhitespace2OffsideRule`
        // (LexFilter.fs:1473-1500) bumps the guard for
        // `ODUMMY TokenRExprParen`. `isSemiSemi` short-circuits
        // indentation entirely.
        if let Some(Context::MemberHead { pos }) = self.head().cloned() {
            let is_semi_semi = matches!(&tt.token, TokenContent::Real(Token::SemiSemi));
            if is_semi_semi
                || tt.start.col + u32::from(Self::is_relax_whitespace2_offside_rule(&tt.token))
                    <= pos.col
            {
                self.pop_ctxt();
                self.delay_token(tt);
                return Step::Restart;
            }
        }
        Step::Pass(tt)
    }
}
