//! Head-context balance and force-closure classifiers — the predicates the
//! offside loop consults when deciding whether an incoming token closes
//! the top-of-stack context, and the end-token mapping that turns those
//! closures into virtual tokens. Mirrors FCS's `tokenBalancesHeadContext`
//! (LexFilter.fs:1260), `suffixExists` (LexFilter.fs:1258),
//! `tokenForcesHeadContextClosure` (LexFilter.fs:1552), `endTokenForACtxt`
//! (LexFilter.fs:1523), and
//! `thereIsACtxtMemberBodyOnTheStackAndWeShouldPopStackForUpcomingMember`
//! (LexFilter.fs:1510-1521).

use super::{AddBlockEnd, Context, Filter, Opener, TokenContent, TokenTup, Virtual};
use crate::lexer::{InterpKind, LexError, Span, Token};

impl<'a, I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>> Filter<'a, I> {
    /// FCS's `tokenBalancesHeadContext` (LexFilter.fs:1260) — does `tt`
    /// "match" the head context, so it should be swallowed rather than
    /// pass through? Only the arms exercised by ported rules are populated.
    pub(super) fn token_balances_head_context(&self, tt: &TokenTup<'a>, stack: &[Context]) -> bool {
        // FCS LexFilter.fs:1268 brace-shape: WITH balances `CtxtSeqBlock ::
        // CtxtParen(LBRACE | LBRACE_BAR) :: _` — i.e. an inner `with` inside
        // a record-update / object expression like `{ r with A = 1 }` must
        // *not* force-close the surrounding match/let. Checked before the
        // single-head match so it can inspect the top two stack entries.
        if matches!(&tt.token, TokenContent::Real(Token::With))
            && let [
                ..,
                Context::Paren {
                    opener: Opener::Brace | Opener::BraceBar,
                    ..
                },
                Context::SeqBlock { .. },
            ] = stack
        {
            return true;
        }
        // FCS LexFilter.fs:1284-1289: `;;` balances `CtxtSeqBlock ::
        // CtxtNamespaceBody :: _` and `CtxtSeqBlock :: CtxtModuleBody(_, true)
        // :: _`. Force-closure must not pop past a namespace body or a
        // whole-file module body when a top-level `;;` arrives — those
        // contexts are the file-wide host and `;;` is a sibling separator
        // in their inner SeqBlock, not a terminator.
        if matches!(&tt.token, TokenContent::Real(Token::SemiSemi)) {
            if let [.., Context::NamespaceBody { .. }, Context::SeqBlock { .. }] = stack {
                return true;
            }
            if let [
                ..,
                Context::ModuleBody {
                    whole_file: true, ..
                },
                Context::SeqBlock { .. },
            ] = stack
            {
                return true;
            }
        }
        // IN inside a query-CE brace is the join operator (`detect_join_in_ctxt`,
        // head = `Vanilla` over a brace `Paren`), so it *balances* — FCS folds
        // this into `tokenBalancesHeadContext` at LexFilter.fs:1281. Without it,
        // `token_forces_head_context_closure` would see a deeper `CtxtFor` /
        // `CtxtLetDecl` that an `IN` balances (the arm below), pop the `Vanilla`,
        // and reprocess the `in` down to that context — emitting a raw `In`
        // (treated as the loop's / binding's `in`) instead of letting
        // `in_done_balances` rewrite it to `Virtual::JoinIn`. This is what makes
        // `query { for x in xs do join y in ys on (x = y) }` parse: the join's
        // `in` must survive the enclosing `for … do`. Checked before the head
        // match because the join head is `Vanilla`, which that match does not
        // cover. (LexFilter.fs:1281.)
        if matches!(&tt.token, TokenContent::Real(Token::In)) && Self::detect_join_in_ctxt(stack) {
            return true;
        }
        let Some(head) = stack.last() else {
            return false;
        };
        // Will grow into a multi-arm match (END/DONE/WITH/FINALLY, record-
        // bracket cases, parenTokensBalance) as those rules land.
        match (&tt.token, head) {
            (TokenContent::Real(Token::In), Context::LetDecl { .. } | Context::For { .. }) => true,
            (TokenContent::Real(Token::Else | Token::Elif), Context::If { .. }) => true,
            // DONE balances CtxtDo (LexFilter.fs:1264) — even a non-offside
            // `done` closes its `do`.
            (TokenContent::Real(Token::Done), Context::Do { .. }) => true,
            // WITH balances CtxtMatch / CtxtException / CtxtTry /
            // CtxtTypeDefns / CtxtMemberHead / CtxtMemberBody /
            // CtxtInterfaceHead (LexFilter.fs:1266). The brace-shape
            // `[.., Paren(LBRACE|LBRACE_BAR), SeqBlock]` alternative is
            // handled by the early-return shim above (mirroring the
            // `| WITH, CtxtSeqBlock :: CtxtParen((LBRACE _ | LBRACE_BAR),
            // _) :: _` arm of FCS's tokenBalancesHeadContext).
            //
            // Without these arms, `match <subexpr> with …` (and the analogue
            // shapes for `try … with`, `type T with`, `member x.P with
            // get…`) where the scrutinee/body leaves intermediate contexts
            // (e.g. an inner SeqBlock for an `if … then … else …`) wouldn't
            // force-close down to the host context and the dedicated WITH+
            // (host) dispatch above (FCS L2362) would never fire.
            (
                TokenContent::Real(Token::With),
                Context::Match { .. }
                | Context::Exception { .. }
                | Context::Try { .. }
                | Context::TypeDefns { .. }
                | Context::MemberHead { .. }
                | Context::MemberBody { .. }
                | Context::InterfaceHead { .. },
            ) => true,
            // FINALLY balances CtxtTry (LexFilter.fs:1269). The only
            // continuator for CtxtTry besides WITH; `try … finally …` parses
            // the same way.
            (TokenContent::Real(Token::Finally), Context::Try { .. }) => true,
            // parenTokensBalance pairs (LexFilter.fs:408). Each closer matches
            // only the CtxtParen pushed by its corresponding opener.
            (
                TokenContent::Real(Token::RParen),
                Context::Paren {
                    opener: Opener::Paren,
                    ..
                },
            ) => true,
            (
                TokenContent::Real(Token::RBrace),
                Context::Paren {
                    opener: Opener::Brace,
                    ..
                },
            ) => true,
            (
                TokenContent::Real(Token::RBrack),
                Context::Paren {
                    opener: Opener::Brack,
                    ..
                },
            ) => true,
            (
                TokenContent::Real(Token::BarRBrack),
                Context::Paren {
                    opener: Opener::BrackBar,
                    ..
                },
            ) => true,
            (
                TokenContent::Real(Token::BarRBrace),
                Context::Paren {
                    opener: Opener::BraceBar,
                    ..
                },
            ) => true,
            // BEGIN/CLASS/SIG/STRUCT/INTERFACE all close with END per
            // parenTokensBalance (LexFilter.fs:414-424).
            (
                TokenContent::Real(Token::End),
                Context::Paren {
                    opener:
                        Opener::Begin | Opener::Sig | Opener::Class | Opener::Struct | Opener::Interface,
                    ..
                },
            ) => true,
            // END balances CtxtWithAsAugment (LexFilter.fs:1262). The
            // augmentation block is the only with-shape where END is a
            // legal closer; for CtxtWithAsLet (record-update / object-
            // expression `with`) the closer is `}` or `|}`. Because END
            // *balances* the augment block, `tokenForcesHeadContextClosure`
            // (L1556) excludes END from forcing closure here — the
            // dedicated arm at L1717-1722 instead pops and emits OEND.
            (TokenContent::Real(Token::End), Context::WithAsAugment { .. }) => true,
            // LQUOTE q1, RQUOTE q2 when q1 = q2 (LexFilter.fs:425): typed and
            // untyped quotations have separate openers so the balance is exact.
            (
                TokenContent::Real(Token::RQuote),
                Context::Paren {
                    opener: Opener::Quote,
                    ..
                },
            ) => true,
            (
                TokenContent::Real(Token::RQuoteRaw),
                Context::Paren {
                    opener: Opener::QuoteRaw,
                    ..
                },
            ) => true,
            // Typar angle: `Less(true)` opened CtxtParen with TyparAngle;
            // `Greater(true)` from the matching scan closes it.
            (
                TokenContent::Real(Token::Greater(true)),
                Context::Paren {
                    opener: Opener::TyparAngle,
                    ..
                },
            ) => true,
            // `InterpString(End)` and `InterpString(Part)` both balance the
            // `CtxtParen(InterpFill)` they close — see FCS
            // `parenTokensBalance` (LexFilter.fs:418-421):
            //   INTERP_STRING_BEGIN_PART, INTERP_STRING_{END,PART}
            //   INTERP_STRING_PART,       INTERP_STRING_{END,PART}
            // Our `InterpFill` opener subsumes both BEGIN_PART and PART.
            (
                TokenContent::Real(Token::InterpString(InterpKind::End { .. } | InterpKind::Part)),
                Context::Paren {
                    opener: Opener::InterpFill,
                    ..
                },
            ) => true,
            _ => false,
        }
    }

    /// FCS's `suffixExists` (LexFilter.fs:1258): true if `p` holds for any
    /// strict suffix (tail) of `stack`.
    pub(super) fn suffix_exists_balances(&self, tt: &TokenTup<'a>, stack: &[Context]) -> bool {
        let mut i = 1;
        while i <= stack.len() {
            if self.token_balances_head_context(tt, &stack[..stack.len() - i]) {
                return true;
            }
            i += 1;
        }
        false
    }

    /// FCS's `tokenForcesHeadContextClosure` (LexFilter.fs:1552). EOF
    /// unconditionally closes the head; `IN` (and friends, when ported)
    /// only close if they *don't* balance the head but balance some deeper
    /// context — otherwise they'd fall through to the parser as a stray
    /// token and trip recovery.
    pub(super) fn token_forces_head_context_closure(&self, tt: &TokenTup<'a>) -> bool {
        if self.offside_stack.is_empty() {
            return false;
        }
        match &tt.token {
            TokenContent::Eof => true,
            // IN, ELSE, ELIF, DONE, FINALLY all share the same shape
            // (LexFilter.fs:1557): close everything until we hit the
            // context they balance. The intermediate ctxts (RHS SeqBlock
            // for IN; then-body SeqBlock + CtxtThen for ELSE; body SeqBlock
            // for DONE; try-body SeqBlock for FINALLY+CtxtTry) get popped on
            // the way. WITH falls into the next arm below (a shim makes it
            // refuse to pop through a MatchClauses head; see #17 history).
            // INTERP_STRING_* follow the same rule but require their balance
            // arms to land first.
            TokenContent::Real(
                Token::In
                | Token::Else
                | Token::Elif
                | Token::Done
                | Token::Finally
                | Token::RParen
                | Token::RBrace
                | Token::RBrack
                | Token::BarRBrack
                | Token::BarRBrace
                | Token::End
                | Token::RQuote
                | Token::RQuoteRaw
                | Token::InterpString(InterpKind::End { .. } | InterpKind::Part),
            ) => {
                !self.token_balances_head_context(tt, &self.offside_stack)
                    && self.suffix_exists_balances(tt, &self.offside_stack)
            }
            // WITH follows the same shape as the previous arm now that
            // CtxtTry is in place: WITH balances CtxtMatch and CtxtTry, so
            // an inner `with` inside `try … with` (e.g. `match x with | _ ->
            // try f x with _ -> 0`) stops force-closure at the inner CtxtTry
            // and never reaches the outer CtxtMatchClauses. Earlier, before
            // CtxtTry existed, this arm carried a defensive shim refusing to
            // pop through MatchClauses — see git history for context.
            TokenContent::Real(Token::With) => {
                !self.token_balances_head_context(tt, &self.offside_stack)
                    && self.suffix_exists_balances(tt, &self.offside_stack)
            }
            // `Greater(true)` is `TokenRExprParen` in FCS: force-close any
            // inner SeqBlock/Fun/Vanilla so the CtxtParen(TyparAngle) at the
            // typar opener is at the head when we reach the closer arm above.
            TokenContent::Real(Token::Greater(true)) => {
                !self.token_balances_head_context(tt, &self.offside_stack)
                    && self.suffix_exists_balances(tt, &self.offside_stack)
            }
            // SEMICOLON_SEMICOLON (LexFilter.fs:1556): `;;` forces closure
            // through every non-balanced head — `CtxtParen`, non-block
            // `CtxtLetDecl`, etc. that have no per-arm `isSemiSemi` clause —
            // until the stack reaches one of the balance arms in
            // `token_balances_head_context` (namespace/whole-file-module
            // body) or empties. Unlike the IN/ELSE/... arm, FCS does NOT
            // require `suffix_exists_balances` — `;;` pops unconditionally
            // when not balanced, even all the way to an empty stack.
            TokenContent::Real(Token::SemiSemi) => {
                !self.token_balances_head_context(tt, &self.offside_stack)
            }
            _ => false,
        }
    }

    /// FCS's `endTokenForACtxt` (LexFilter.fs:1523).
    pub(super) fn end_token_for_a_ctxt(&self, ctxt: &Context) -> Option<Virtual> {
        match ctxt {
            Context::SeqBlock {
                add_block_end: AddBlockEnd::Yes,
                ..
            } => Some(Virtual::BlockEnd),
            Context::SeqBlock {
                add_block_end: AddBlockEnd::OneSided,
                ..
            } => Some(Virtual::RightBlockEnd),
            Context::SeqBlock {
                add_block_end: AddBlockEnd::No,
                ..
            } => None,
            Context::LetDecl {
                block_let: true, ..
            } => Some(Virtual::DeclEnd),
            Context::LetDecl {
                block_let: false, ..
            } => None,
            Context::Do { .. } => Some(Virtual::DeclEnd),
            Context::For { .. } => None,
            // CtxtFun closes with OEND. (LexFilter.fs:1525)
            Context::Fun { .. } => Some(Virtual::End),
            // CtxtMatchClauses also closes with OEND. (LexFilter.fs:1526)
            Context::MatchClauses { .. } => Some(Virtual::End),
            // CtxtWithAsLet closes with OEND. (LexFilter.fs:1527)
            Context::WithAsLet { .. } => Some(Virtual::End),
            // CtxtIf/CtxtThen/CtxtElse/CtxtFor/CtxtWhile/CtxtMatch/CtxtWhen
            // all fall into FCS's catch-all `_ -> None` arm
            // (LexFilter.fs:1545) — they pop silently and rely on their
            // inner SeqBlock(AddBlockEnd) for any actual virtual token.
            // (CtxtFor already handled above.)
            // CtxtFunction also falls into the `_ -> None` arm
            // (LexFilter.fs:1545) — it pops silently. The companion
            // CtxtMatchClauses underneath emits the OEND that surfaces.
            Context::If { .. }
            | Context::Then { .. }
            | Context::Else { .. }
            | Context::While { .. }
            | Context::Match { .. }
            | Context::When { .. }
            | Context::Vanilla { .. }
            | Context::Paren { .. }
            | Context::Try { .. }
            | Context::Function { .. }
            | Context::Exception { .. }
            | Context::InterfaceHead { .. } => None,
            // CtxtModuleHead(isNested=true) → OBLOCKSEP on force-closure
            // (LexFilter.fs:1542-1543). Top-of-file module heads
            // (isNested=false) and namespace heads fall through silently.
            Context::ModuleHead { nested: true, .. } => Some(Virtual::BlockSep),
            Context::ModuleHead { nested: false, .. }
            | Context::NamespaceHead { .. }
            | Context::NamespaceBody { .. }
            | Context::ModuleBody { .. }
            | Context::TypeDefns { .. } => None,
            // MemberHead/MemberBody fall into FCS's catch-all `_ -> None`
            // (LexFilter.fs:1545). MemberBody's `ODECLEND` is emitted by
            // the offside-pop arm directly (LexFilter.fs:2005), and the
            // multi-member pop-loop (LexFilter.fs:2193) pops MemberBody
            // explicitly without consulting this function.
            Context::MemberHead { .. } | Context::MemberBody { .. } => None,
            // CtxtWithAsAugment maps to `ODECLEND` per FCS L1530-1534.
            // The END+WithAsAugment balance arm (L1717-1722) emits OEND
            // directly via its own return path — it does not go through
            // this function. The offside-pop path (L2025-2029) also emits
            // ODECLEND directly without consulting this function. The
            // remaining caller is the MEMBER pop-cascade at L2185 (when
            // STATIC/MEMBER/etc arrives inside a CtxtMemberBody and we
            // unwind through any intervening WithAsAugment).
            Context::WithAsAugment { .. } => Some(Virtual::DeclEnd),
        }
    }

    /// FCS's `thereIsACtxtMemberBodyOnTheStackAndWeShouldPopStackForUpcomingMember`
    /// (LexFilter.fs:1510-1521). A `VAL`/`STATIC`/`ABSTRACT`/`MEMBER`/
    /// `OVERRIDE`/`DEFAULT` keyword should pop contexts down to the
    /// enclosing `CtxtMemberBody` *unless* we might be inside one of two
    /// constructs whose body can legally contain such a keyword without
    /// terminating the surrounding member:
    ///
    /// * an object expression (`{ new I with member ... }`) — flagged by
    ///   an `LBRACE`-opener `CtxtParen` anywhere on the stack;
    /// * a static inline constraint (`when 'T : (static member …)`) —
    ///   flagged by at least two `LPAREN`-opener `CtxtParen`s on the stack.
    ///
    /// FCS errs conservatively: when in doubt we DON'T pop, so legal code
    /// keeps parsing. We mirror that exactly.
    pub(super) fn there_is_a_ctxt_member_body_on_the_stack_and_we_should_pop(&self) -> bool {
        let stack = self.offside_stack.as_slice();
        if !stack
            .iter()
            .any(|c| matches!(c, Context::MemberBody { .. }))
        {
            return false;
        }
        // FCS L1516 matches `LBRACE _` only (regular `{`, parameterised by
        // the brace body-kind), not `LBRACE_BAR` (`{|`). The body-kind
        // disambiguates anon-record vs computation expression, but both
        // shapes can legally contain `member …` (object expressions live
        // inside `{ new I with member … }`).
        if stack.iter().any(|c| {
            matches!(
                c,
                Context::Paren {
                    opener: Opener::Brace,
                    ..
                }
            )
        }) {
            return false;
        }
        let lparen_count = stack
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    Context::Paren {
                        opener: Opener::Paren,
                        ..
                    }
                )
            })
            .count();
        if lparen_count >= 2 {
            return false;
        }
        true
    }
}
