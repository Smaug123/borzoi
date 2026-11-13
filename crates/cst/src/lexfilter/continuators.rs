//! Continuator predicates — pure, static-method classifiers asking "may this
//! token align with the head context's column without closing it?". Each
//! method mirrors one of FCS's `is*Continuator` helpers; the offside-pop
//! guards in `hw_token_fetch` consult them to decide whether to bump the
//! comparison from `tokenStartCol <= offsidePos` to `+1 <=`. Split out into
//! its own file because the 14 predicates are uniform-shape and unrelated to
//! the surrounding `Filter` state — they don't touch `self`, only the token
//! they're asked about.

use super::{DummyInner, Filter, TokenContent, TokenTup, Virtual};
use crate::lexer::TokenKind;

impl<'a> Filter<'a> {
    /// FCS's `isLetContinuator` (LexFilter.fs:336). When the incoming token may
    /// align with the `let`'s column without closing the `CtxtLetDecl`, the
    /// LetDecl offside-pop guard bumps from `tokenStartCol <= offsidePos` to
    /// `+1 <=` — keeping the binding open for mutually-recursive `and`
    /// continuations and for the reprocessed virtual endings synthesised
    /// by upstream balance arms (e.g. DONE→ODECLEND via the CtxtDo balance).
    /// Without the virtual cases, a `done` aligned with `let` would pop
    /// CtxtLetDecl on the reprocessed ODECLEND and emit a duplicate ODECLEND
    /// at `done`'s range rather than at EOF.
    pub(super) fn is_let_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_let_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(TokenKind::And)
                | TokenContent::Virtual(
                    Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
                )
        )
    }

    /// FCS's `isTypeContinuator` (LexFilter.fs:288). Tokens that may align
    /// with the `type` keyword's column without closing the surrounding
    /// `CtxtTypeDefns`. `AND` allows mutually-recursive `type T = … and U =
    /// …`; `BAR` keeps the construct open across leading-bar DU arms
    /// aligned with `type`; `END` closes paired `struct`/`interface`/`sig`
    /// bodies (parenTokensBalance pairs at LexFilter.fs:414-424) — we don't
    /// push those Opener variants yet but the continuator entry is harmless
    /// when no such Paren is on the stack. `WITH` aligned with `type` is the
    /// `type T = …\nend with\n  member …` augmentation shape; without
    /// CtxtMember* contexts the WITH cases never benefit from this guard,
    /// but again it's inert when the WITH dispatch doesn't fire. `RBrace`
    /// is the closing `}` of a record-type body (`type T = { x: int\n}`)
    /// aligned with `type`. Virtual reprocessed endings are the same set
    /// as `is_let_continuator` for the same reason — a virtual block-end
    /// cascading up to a CtxtTypeDefns must not be treated as offside.
    pub(super) fn is_type_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_type_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(
                TokenKind::And
                    | TokenKind::Bar
                    | TokenKind::End
                    | TokenKind::With
                    | TokenKind::RBrace,
            ) | TokenContent::Virtual(
                Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
            )
        )
    }

    /// FCS's `isTypeSeqBlockElementContinuator` (LexFilter.fs:346-357). A
    /// sequence of items separated by `|` counts as one sequence-block
    /// element in a type definition, so the inner SeqBlock anchored at the
    /// first DU arm shouldn't be popped by `|` aligned with the type-level
    /// SeqBlock column. Used by the BAR grace=2 (L1813) and the grace=-1
    /// special case (L1823) — both gate on this predicate to keep the
    /// SeqBlock alive across leading-bar arms.
    pub(super) fn is_type_seq_block_element_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_type_seq_block_element_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(TokenKind::Bar)
                | TokenContent::Virtual(
                    Virtual::BlockBegin
                        | Virtual::BlockEnd
                        | Virtual::RightBlockEnd
                        | Virtual::DeclEnd,
                )
        )
    }

    /// FCS's `isIfBlockContinuator` (LexFilter.fs:202). Tokens that may align
    /// with the `if`'s column without closing the `CtxtIf`. The full FCS list
    /// is THEN | ELSE | ELIF | END | RPAREN plus the virtual reprocessed
    /// tokens (ORIGHT_BLOCK_END / OBLOCKEND / ODECLEND) and recursion under
    /// ODUMMY. The virtual cases matter once DONE handling lands: a
    /// `done` closing a nested `do` inside an `if … then` branch generates
    /// a virtual `ODECLEND` that cascades up through the surrounding
    /// `CtxtIf`; without recognising the virtual we'd pop the conditional
    /// before a trailing `else` can balance it.
    pub(super) fn is_if_block_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_if_block_continuator(&inner.content());
        }
        // FCS LexFilter.fs:202 also lists END and RPAREN as continuators. END
        // prevents CtxtIf from popping when `end` is aligned with `if` (e.g.
        // `if c then begin ... end`). RPAREN is also continuator but any RPAREN
        // that might reach this guard has already been force-closed by the
        // CtxtParen mechanism above, so adding it here is harmless but inert.
        matches!(
            token,
            TokenContent::Real(
                TokenKind::Then | TokenKind::Else | TokenKind::Elif | TokenKind::End
            ) | TokenContent::Virtual(
                Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
            )
        )
    }

    /// FCS's `isThenBlockContinuator` (LexFilter.fs:247-252). The continuator
    /// is purely the reprocessed virtual endings — no real keywords align
    /// with `then` without closing it. Without this, a `done` closing a
    /// nested `do` inside `then` cascades through `CtxtThen`, popping it on
    /// the reprocessed virtual; the next aligned `else` then finds no
    /// surrounding `CtxtIf` to balance against (it has been popped too) and
    /// the filtered stream gains a spurious `OffsideBlockSep`.
    pub(super) fn is_then_block_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_then_block_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Virtual(Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,)
        )
    }

    /// FCS's `isWhileBlockContinuator` (LexFilter.fs:325). Same shape as
    /// the for-loop and do continuators: `DONE` plus the reprocessed virtual
    /// endings. `CtxtWhile` pops silently (no virtual emitted), so this
    /// rarely changes the observable token stream — but for FCS parity, and
    /// to match the surrounding for/do/if/then continuator extensions made
    /// for DONE reprocessing, the virtual cases belong here too.
    pub(super) fn is_while_block_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_while_block_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(TokenKind::Done)
                | TokenContent::Virtual(
                    Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
                )
        )
    }

    /// FCS's `isForLoopContinuator` (LexFilter.fs:314). Tokens that may align
    /// with `for`'s column without closing the `CtxtFor`. `DONE` is the only
    /// real keyword on the list; the rest are the reprocessed virtual endings
    /// that surface when DONE handling delays an `ODECLEND`. Without the
    /// virtual cases, an aligned `done` (replaced by `Virtual::DeclEnd`) pops
    /// the surrounding `CtxtFor`, which lets later tokens (e.g. a real `in`)
    /// hit the wrong balance arm.
    pub(super) fn is_for_loop_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_for_loop_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(TokenKind::Done)
                | TokenContent::Virtual(
                    Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
                )
        )
    }

    /// FCS's `isMatchBlockContinuator` (LexFilter.fs:223). Tokens that may
    /// align with `match`'s column without closing the `CtxtMatch`. `WITH`
    /// is the only real keyword on the list (so `match …\n with …` keeps
    /// CtxtMatch open until WITH balances it). The virtual reprocessed
    /// endings cover the same DONE-cascade case as the other continuator
    /// predicates.
    pub(super) fn is_match_block_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_match_block_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(TokenKind::With)
                | TokenContent::Virtual(
                    Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
                )
        )
    }

    /// FCS's `isTryBlockContinuator` (LexFilter.fs:236). Aligned `with` or
    /// `finally` keeps the surrounding `CtxtTry` open until the balance arm
    /// fires; reprocessed virtual endings (DONE-cascade) likewise.
    pub(super) fn is_try_block_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_try_block_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(TokenKind::With | TokenKind::Finally)
                | TokenContent::Virtual(
                    Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
                )
        )
    }

    /// FCS's `isDoContinuator` (LexFilter.fs:254). Same shape as the for-loop
    /// and while-block continuators: `DONE` plus the reprocessed virtual
    /// endings. Used at the `CtxtDo` offside-pop guard to keep an outer
    /// `do` open when a nested loop's `done` reprocesses an aligned virtual
    /// past it (e.g. nested `do ... do ... done` where the inner `done` lands
    /// at the outer `do`'s column).
    pub(super) fn is_do_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_do_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(TokenKind::Done)
                | TokenContent::Virtual(
                    Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
                )
        )
    }

    /// FCS's `isWithAugmentBlockContinuator` (LexFilter.fs:383-392). A token
    /// at the `CtxtWithAsAugment`'s aligned column that continues the
    /// augmentation block rather than closing it. Only `END` qualifies —
    /// the canonical shape is
    ///
    /// ```text
    /// interface Foo
    ///    with
    ///       member ...
    ///    end
    /// ```
    ///
    /// where the `end` aligns with the `with`. END is in turn force-closed
    /// by `token_forces_head_context_closure`, so this predicate keeps
    /// WithAsAugment open just long enough for the force-closure path to
    /// emit OEND through `end_token_for_a_ctxt`.
    pub(super) fn is_with_augment_block_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_with_augment_block_continuator(&inner.content());
        }
        matches!(token, TokenContent::Real(TokenKind::End))
    }

    /// FCS's `isInterfaceContinuator` (LexFilter.fs:266-275). An aligned
    /// `end` keeps the `CtxtInterfaceHead` open so the explicit closer
    /// reaches the inner `CtxtWithAsAugment` (which then closes via the
    /// END balance arm and emits `OEND`). Reprocessed virtual endings
    /// (DONE-cascade tail) likewise keep the head open until the
    /// surrounding force-closure dispatch fires.
    pub(super) fn is_interface_continuator(token: &TokenContent) -> bool {
        if let TokenContent::Dummy { inner, .. } = token {
            return Self::is_interface_continuator(&inner.content());
        }
        matches!(
            token,
            TokenContent::Real(TokenKind::End)
                | TokenContent::Virtual(
                    Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
                )
        )
    }

    /// FCS's `isSeqBlockElementContinuator` (LexFilter.fs:360). A token at the
    /// SeqBlock's aligned column that continues the prior expression rather than
    /// starting a new statement. OBLOCKSEP is suppressed for these.
    ///
    /// The predicate is `isInfix token || <explicit list>` (LexFilter.fs:360-381).
    pub(super) fn is_seq_block_element_continuator(&self, tt: &TokenTup) -> bool {
        if let TokenContent::Dummy { inner, .. } = tt.token {
            return self.is_seq_block_element_continuator(&TokenTup {
                token: inner.content(),
                ..*tt
            });
        }
        // `isInfix token || <explicit list>` (LexFilter.fs:360-381). The infix
        // half is shared with [`Self::token_is_infix`].
        if self.token_is_infix(tt) {
            return true;
        }
        match tt.token {
            TokenContent::Real(t) => matches!(
                t,
                // Non-infix continuators (LexFilter.fs:376): closing tokens
                // and keywords that close or continue the current expression.
                TokenKind::End
                    | TokenKind::And
                    | TokenKind::With
                    | TokenKind::Then
                    | TokenKind::RParen
                    | TokenKind::RBrace
                    | TokenKind::RBrack
                    | TokenKind::BarRBrack
                    | TokenKind::BarRBrace
                    | TokenKind::RQuote
                    | TokenKind::RQuoteRaw
            ),
            // Virtual reprocessed endings (LexFilter.fs:378-381):
            // ORIGHT_BLOCK_END / OBLOCKEND / ODECLEND arriving from a
            // prior alignment-driven pop (e.g. DONE→ODECLEND via the
            // CtxtDo balance arm) continue the surrounding SeqBlock
            // rather than starting a new statement.
            TokenContent::Virtual(
                Virtual::BlockEnd | Virtual::RightBlockEnd | Virtual::DeclEnd,
            ) => true,
            _ => false,
        }
    }

    /// FCS's `isInfix` (LexFilter.fs:118-151) of `tt`: see [`kind_is_infix`].
    pub(super) fn token_is_infix(&self, tt: &TokenTup) -> bool {
        match tt.token {
            TokenContent::Real(kind) => kind_is_infix(kind, || self.text(tt)),
            TokenContent::Dummy { inner, .. } => self.token_is_infix(&TokenTup {
                token: inner.content(),
                ..*tt
            }),
            _ => false,
        }
    }

    /// FCS's `infixTokenLength` (LexFilter.fs:153-179): the column width of an
    /// infix token, used for the SeqBlock offside grace (`infixTokenLength + 1`,
    /// LexFilter.fs:1854) that lets a leading infix operator sit left of the
    /// block's anchor without closing it. Only meaningful for tokens that
    /// [`Self::token_is_infix`] accepts; an operator's length is its source width.
    pub(super) fn infix_token_length(&self, tt: &TokenTup) -> u32 {
        match tt.token {
            TokenContent::Dummy { inner, .. } => self.infix_token_length(&TokenTup {
                token: inner.content(),
                ..*tt
            }),
            TokenContent::Real(TokenKind::Op) => self.text(tt).chars().count() as u32,
            TokenContent::Real(
                TokenKind::Comma | TokenKind::Amp | TokenKind::Or | TokenKind::Dollar,
            ) => 1,
            TokenContent::Real(
                TokenKind::BarBar
                | TokenKind::AmpAmp
                | TokenKind::ColonColon
                | TokenKind::ColonGreater
                | TokenKind::ColonEquals
                | TokenKind::QMarkQMark,
            ) => 2,
            // `:?>` (COLON_QMARK_GREATER); `mod` is `INFIX_STAR_DIV_MOD_OP "mod"`.
            TokenContent::Real(TokenKind::ColonQMarkGreater | TokenKind::Mod) => 3,
            _ => 1,
        }
    }

    /// Whether `tt` is an operator that FCS's lexer relexes to
    /// `ADJACENT_PREFIX_OP` when it is glued to the following token — a
    /// term-starting *prefix* use rather than an infix one. FCS's set (per
    /// `lex.fsl` / `PrettyNaming`) is `+`, `-`, `+.`, `-.`, `%`, `%%`, `&`, `&&`
    /// (note: *not* `&&&`/`|||`/`*`/`/`, which are infix-only). `%`/`%%` and the
    /// `~`/`!`-prefixed ops already classify as non-infix in
    /// [`op_str_is_infix`], so only the ops that would otherwise be infix
    /// continuators need listing here: `+`/`-`/`+.`/`-.` and the dedicated
    /// `Amp`/`AmpAmp` tokens.
    ///
    /// The *adjacency* itself is checked at the call site (the offside
    /// `OffsideBlockSep` rule) via `is_adjacent` — this predicate only says the
    /// op *can* be an adjacent prefix. Our lexer emits one `Op`/`Amp`/`AmpAmp`
    /// token regardless of spacing (sign-folding happens later), so the offside
    /// filter reconstructs FCS's adjacency distinction itself.
    pub(super) fn is_adjacent_prefix_capable_op(&self, tt: &TokenTup) -> bool {
        match tt.token {
            TokenContent::Dummy { inner, .. } => self.is_adjacent_prefix_capable_op(&TokenTup {
                token: inner.content(),
                ..*tt
            }),
            TokenContent::Real(TokenKind::Amp | TokenKind::AmpAmp) => true,
            TokenContent::Real(TokenKind::Op) => {
                matches!(self.text(tt), "+" | "-" | "+." | "-.")
            }
            _ => false,
        }
    }
    /// The token half of FCS's `relaxWhitespace2OffsideRule`
    /// (LexFilter.fs:1473-1500): whether the current token is an `ODUMMY`
    /// wrapping a `TokenRExprParen` (LexFilter.fs:194-198: END / RPAREN /
    /// RBRACE / BAR_RBRACE / RBRACK / BAR_RBRACK / RQUOTE / GREATER true).
    /// FCS's rule is this predicate *and* `relaxWhitespace2` (F# 6.0+); the
    /// offside-pop arms consult the conjunction through
    /// [`Filter::is_offside_of`], which also records when the version half
    /// decides the outcome.
    ///
    /// The `ODUMMY TokenRExprParen` itself is synthesised at three call
    /// sites in `mod.rs`: the IN+CtxtLetDecl arm queues `Dummy(In)` (not a
    /// `TokenRExprParen`, so this predicate returns false for it); the
    /// paren-balance arm queues `Dummy(closer)` for every
    /// `TokenRExprParen` closing a `CtxtParen`; and the
    /// END+CtxtWithAsAugment arm queues `Dummy(End)` (`End` is in
    /// `TokenRExprParen`, so this predicate fires).
    pub(super) fn is_rexpr_paren_dummy(token: &TokenContent) -> bool {
        let TokenContent::Dummy { inner, .. } = token else {
            return false;
        };
        matches!(
            inner,
            DummyInner::Real(
                TokenKind::End
                    | TokenKind::RParen
                    | TokenKind::RBrace
                    | TokenKind::BarRBrace
                    | TokenKind::RBrack
                    | TokenKind::BarRBrack
                    | TokenKind::RQuote
                    | TokenKind::RQuoteRaw
            ) | DummyInner::TyparGreater
        )
    }
}

/// FCS's `isInfix` (LexFilter.fs:118-151) of a lexed token of kind `kind`
/// whose text `text` returns: the infix operators and punctuation that, when
/// they end a line, continue the expression onto the next line (the "r.h.s. of
/// an infix token begins a new block" rule, LexFilter.fs:2330 — see
/// [`Filter::infix_rhs_pushes`]). `text` is asked for only for an operator.
///
/// FCS deliberately **excludes** `<`, `>`, and `=` (LexFilter.fs:129-138):
/// treating them as infix would conflict with `f<int>` and `let f x = …`. In
/// our token model bare `<`/`>`/`=` are the dedicated `Token::Less` /
/// `Token::Greater` / `Token::Equals` (never `Token::Op`), so they simply do
/// not appear below; multi-char `<=`/`>=`/`<<<` arrive as `Token::Op` and
/// *are* infix. Also excluded: `BANG`, `PREFIX_OP` (`!`-/`~`-prefixed), and
/// `PERCENT_OP` (`%`/`%%`) — all handled by [`op_str_is_infix`].
pub(super) fn kind_is_infix<'t>(kind: TokenKind, text: impl FnOnce() -> &'t str) -> bool {
    match kind {
        TokenKind::Op => op_str_is_infix(text()),
        TokenKind::Comma        // COMMA
        | TokenKind::BarBar     // BAR_BAR
        | TokenKind::AmpAmp     // AMP_AMP
        | TokenKind::Amp        // AMP (single &)
        | TokenKind::Or         // OR keyword
        | TokenKind::Mod        // MOD keyword → INFIX_STAR_DIV_MOD_OP
        | TokenKind::ColonColon     // COLON_COLON
        | TokenKind::ColonGreater   // COLON_GREATER `:>`
        | TokenKind::ColonQMarkGreater // COLON_QMARK_GREATER `:?>`
        | TokenKind::ColonEquals    // COLON_EQUALS
        | TokenKind::QMarkQMark // QMARK_QMARK `??`
        | TokenKind::Dollar => true, // DOLLAR `$`
        _ => false,
    }
}

/// Whether an operator string maps to FCS's `isInfix` categories.
/// Strips leading FCS `ignored_op_chars` (`.`, `?`) — but NOT `$`, because
/// `$` is simultaneously an ignored_op_char and an INFIX_COMPARE_OP head
/// character, so the lexer classifies `$!`/`.$!` etc. as INFIX_COMPARE_OP.
/// Non-infix: PREFIX_OP (`!`-prefixed except `!=`, all `~`-prefixed),
/// PERCENT_OP (`%`/`%%`). Single `<`/`>` are dedicated `Token::Less`/
/// `Token::Greater` variants and never reach this function. Multi-char
/// `<`/`>`-prefixed forms (`<=`, `>=`, `<>`, `>>`, …) do, and are infix.
fn op_str_is_infix(s: &str) -> bool {
    // FCS exact carve-outs that produce non-infix token kinds.
    if matches!(s, "%" | "%%") {
        return false;
    }
    let bytes = s.as_bytes();
    // Strip leading FCS ignored_op_chars. Only `.` and `?` are "purely"
    // ignored; `$` is also an INFIX_COMPARE_OP head so we stop before it.
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b'.' | b'?') {
        i += 1;
    }
    if i >= bytes.len() {
        return false;
    }
    match bytes[i] {
        // Only `!=` → INFIX_COMPARE_OP; all other `!`-prefixed → PREFIX_OP.
        b'!' => bytes.get(i + 1) == Some(&b'='),
        // PREFIX_OP.
        b'~' => false,
        // Everything else (*, /, %, +, -, @, ^, &, |, =, $, <, >, …) → infix.
        _ => true,
    }
}
