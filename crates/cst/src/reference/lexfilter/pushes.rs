//! Context-pushing rules for `hw_token_fetch` — the keyword-triggered half of
//! the dispatch loop.

use super::{
    AddBlockEnd, Context, Filter, ModuleHeadPrev, NamespacePrev, OffsideDiagnostic,
    OffsideSeverity, Opener, Pos, PushStrictness, Step, TokenContent, TokenTup, Virtual,
};
use crate::lexer::{LexError, Span, Token};

/// A declaration keyword that must appear at module/namespace level, not nested
/// inside a type — three of FCS's `checkForInvalidDeclsInTypeDefn` keywords.
/// (`open` is deliberately excluded: the parser already emits its FS0058, see
/// `decls_type.rs`'s `stray_open_in_type_body_span`.) Selects the message.
#[derive(Debug, Clone, Copy)]
pub(super) enum NestedDeclKeyword {
    Type,
    Module,
    Exception,
}

impl NestedDeclKeyword {
    /// FCS's per-keyword message (`FSComp.txt:1004-1006`).
    fn message(self) -> &'static str {
        match self {
            NestedDeclKeyword::Type => {
                "Nested type definitions are not allowed. Types must be defined at module or \
                 namespace level."
            }
            NestedDeclKeyword::Module => {
                "Modules cannot be nested inside types. Define modules at module or namespace level."
            }
            NestedDeclKeyword::Exception => {
                "Exceptions must be defined at module level, not inside types."
            }
        }
    }
}

/// Port of FCS's `checkForInvalidDeclsInTypeDefn` core predicate
/// (`LexFilter.fs:1396-1454`): whether an incoming `type`/`module`/`exception`
/// token starting at `tok` is *inappropriately* nested inside a `CtxtTypeDefns`.
/// `stack` is the offside stack with the **top at the end** (`offside_stack`),
/// so the recursion peels the top via `split_last`.
fn is_invalid_decl_in_type_defn(stack: &[Context], tok: Pos) -> bool {
    // Skip validation inside a paren context — avoids false positives with
    // inline IL, e.g. `(# "unbox.any !0" type ('T) x : 'T #)`.
    if has_paren_context(stack) {
        return false;
    }
    check_nesting(stack, tok, false)
}

/// FCS `hasParenContext`: a `CtxtParen` reachable through only transparent
/// `CtxtSeqBlock`/`CtxtVanilla` from the top.
fn has_paren_context(stack: &[Context]) -> bool {
    match stack.split_last() {
        Some((Context::Paren { .. }, _)) => true,
        Some((Context::SeqBlock { .. } | Context::Vanilla { .. }, rest)) => has_paren_context(rest),
        _ => false,
    }
}

/// FCS `checkNesting`: walk down the stack for a `CtxtTypeDefns` the token is
/// indented inside (a *later* line at a *greater* column — same-line/same-column
/// declarations are sequential, not nested) and not in a member/augmentation
/// context.
fn check_nesting(stack: &[Context], tok: Pos, type_defns_seen: bool) -> bool {
    match stack.split_last() {
        None => false,
        // Escaped to module/namespace level — constructs here are fine.
        Some((Context::ModuleBody { .. } | Context::NamespaceBody { .. }, _)) => false,
        Some((Context::TypeDefns { pos: type_pos, .. }, rest)) => {
            if tok.line > type_pos.line && tok.col > type_pos.col {
                // Indented inside the type — invalid unless in a member/augment
                // context (checked from this `CtxtTypeDefns` node down).
                !is_in_member_context(stack)
            } else {
                // Same column or less — check deeper.
                check_nesting(rest, tok, true)
            }
        }
        Some((
            Context::SeqBlock { .. } | Context::Vanilla { .. } | Context::Paren { .. },
            rest,
        )) => check_nesting(rest, tok, type_defns_seen),
        Some((Context::MemberHead { .. } | Context::MemberBody { .. }, _)) if type_defns_seen => {
            false
        }
        Some((_, rest)) => check_nesting(rest, tok, type_defns_seen),
    }
}

/// FCS `isInMemberContext`: a member head/body or type-augmentation `with`
/// reachable through only transparent `CtxtSeqBlock`/`CtxtVanilla` from the top.
fn is_in_member_context(stack: &[Context]) -> bool {
    match stack.split_last() {
        Some((
            Context::MemberHead { .. } | Context::MemberBody { .. } | Context::WithAsAugment { .. },
            _,
        )) => true,
        Some((Context::SeqBlock { .. } | Context::Vanilla { .. }, rest)) => {
            is_in_member_context(rest)
        }
        _ => false,
    }
}

impl<'a, I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>> Filter<'a, I> {
    /// FCS's `checkForInvalidDeclsInTypeDefn` (`LexFilter.fs:1392-1470`) for
    /// `type`/`module`/`exception`: when the keyword is inappropriately nested
    /// inside a type definition, record the FS0058 error (`errorR` in FCS —
    /// recoverable, so the token/context flow is unchanged; only the diagnostic
    /// is added). Gated on the F# 10 `ErrorOnInvalidDeclsInTypeDefinitions`
    /// feature. (`open` is handled parser-side; see [`NestedDeclKeyword`].)
    fn check_invalid_decl_in_type_defn(&mut self, keyword: NestedDeclKeyword, tok: &TokenTup<'a>) {
        if !self.reports_invalid_decls_in_type {
            return;
        }
        if is_invalid_decl_in_type_defn(&self.offside_stack, tok.start) {
            self.diagnostics.push(OffsideDiagnostic {
                message: keyword.message().to_string(),
                span: tok.span.clone(),
                severity: OffsideSeverity::Error,
            });
        }
    }

    /// NAMESPACE / MODULE / TYPE context pushes.
    pub(super) fn module_type_pushes(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // NAMESPACE → push CtxtNamespaceHead, emit NAMESPACE
        // unchanged. (LexFilter.fs:2117) The head's dotted-ident
        // scanner is consumed by the CtxtNamespaceHead transition arm
        // above; this arm only sets the stack up.
        if let TokenContent::Real(Token::Namespace) = &tt.token {
            self.push_ctxt(
                tt.span.clone(),
                Context::NamespaceHead {
                    pos: tt.start,
                    prev: NamespacePrev::Keyword,
                },
            );
            return Step::Emit(tt);
        }

        // MODULE → push CtxtModuleHead and swallow the token.
        // (LexFilter.fs:2124) `insertComingSoonTokens` queues 6×
        // MODULE_COMING_SOON + MODULE_IS_HERE faux tokens; all map to
        // `FSharpTokenKind.None` and are filtered by the FCS public-
        // API tokenizer (ServiceLexing.fs:1814-1818), so they're
        // invisible in our oracle stream and not emitted here.
        //
        // The expression-context unwind FCS performs inside
        // `insertComingSoonTokens` (LexFilter.fs:1582-1626) — popping
        // open parens / SeqBlocks / Vanillas down to the next
        // namespace-or-module boundary — is a no-op for the cases the
        // corpus actually contains (top-level MODULE arrives with
        // `[CtxtSeqBlock]` or `[CtxtSeqBlock, CtxtModuleBody, ...]` at
        // the head, and the loop guard exits immediately). A
        // pathological MODULE inside an open paren expression isn't
        // supported yet; add the unwind when the corpus needs it.
        //
        // `isNested` is computed at push time from the stack shape
        // and stored on the head for `end_token_for_a_ctxt` (a forced
        // closure of a nested head emits OBLOCKSEP per
        // LexFilter.fs:1542-1543; a top-of-file head closes silently).
        if let TokenContent::Real(Token::Module) = &tt.token {
            self.check_invalid_decl_in_type_defn(NestedDeclKeyword::Module, &tt);
            let nested = !matches!(self.offside_stack.as_slice(), [Context::SeqBlock { .. }]);
            self.push_ctxt(
                tt.span.clone(),
                Context::ModuleHead {
                    pos: tt.start,
                    prev: ModuleHeadPrev::Module,
                    attrs: false,
                    nested,
                },
            );
            self.last_real_end = tt.span.end;
            return Step::Restart;
        }

        // TYPE → push CtxtTypeDefns and swallow the token.
        // (LexFilter.fs:2579-2587) `insertComingSoonTokens` queues
        // 6× TYPE_COMING_SOON + TYPE_IS_HERE faux tokens that map to
        // `FSharpTokenKind.None` and are filtered by the FCS public-
        // API tokenizer (same pattern as MODULE). The expression-
        // context unwind inside `insertComingSoonTokens`
        // (LexFilter.fs:1582-1626) is not modelled — the corpus
        // shapes that exercise TYPE all arrive with a SeqBlock /
        // ModuleBody / TypeDefns head, and the unwind exits
        // immediately for those.
        //
        // `equals_end: None` marks the pre-EQUALS state; the
        // EQUALS+CtxtTypeDefns arm above replaces it with
        // `Some(equalsEnd)` when `=` is reached.
        if let TokenContent::Real(Token::Type) = &tt.token {
            self.check_invalid_decl_in_type_defn(NestedDeclKeyword::Type, &tt);
            self.push_ctxt(
                tt.span.clone(),
                Context::TypeDefns {
                    pos: tt.start,
                    equals_end: None,
                },
            );
            self.last_real_end = tt.span.end;
            return Step::Restart;
        }
        Step::Pass(tt)
    }

    /// let/use, bang-binders, the multi-member pop cascade, and member-head pushes.
    pub(super) fn binding_member_pushes(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // `static let` — LET arriving while the head is `CtxtMemberHead`
        // (i.e. the `static` keyword has already opened a member-head)
        // pops the MemberHead and pushes `CtxtLetDecl(blockLet=true,
        // headPos)` anchored at the member-head's own column rather than
        // LET's column. Emits OLET. (LexFilter.fs:2146-2151.) Must fire
        // before the generic LET arm below so the MemberHead → LetDecl
        // swap happens instead of stacking LetDecl on top.
        if matches!(&tt.token, TokenContent::Real(Token::Let | Token::Use))
            && let Some(Context::MemberHead { pos: head_pos }) = self.head().cloned()
        {
            self.pop_ctxt();
            self.push_ctxt(
                tt.span.clone(),
                Context::LetDecl {
                    block_let: true,
                    pos: head_pos,
                },
            );
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::Let),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }

        // LET / USE both push CtxtLetDecl and emit OLET when the head
        // context is `CtxtSeqBlock | CtxtMatchClauses` (LexFilter.fs:2157-
        // 2163). Otherwise FCS pushes CtxtLetDecl(blockLet=false) and
        // emits the raw LET token; this surfaces for pathological cases
        // like a `let` arriving while the stack still has a CtxtParen
        // (e.g. an unclosed `{ r with A = 1 …`) on top after offside-pops
        // have peeled the inner contexts. The block_let bit drives
        // whether the LetDecl offside-pop emits ODECLEND.
        //
        // FCS's LET token carries an `isUse` bool that distinguishes
        // `let` from `use` at the parser level, but both flow through
        // the same arm here and both surface as
        // `FSharpTokenKind.OffsideLet` (ServiceLexing.fs:1418).
        if matches!(&tt.token, TokenContent::Real(Token::Let | Token::Use)) {
            let pos = tt.start;
            let block_let = matches!(
                self.head(),
                Some(Context::SeqBlock { .. } | Context::MatchClauses { .. })
            );
            self.push_ctxt(tt.span.clone(), Context::LetDecl { block_let, pos });
            if block_let {
                return Step::Emit(TokenTup {
                    token: TokenContent::Virtual(Virtual::Let),
                    span: tt.span,
                    start: tt.start,
                    end: tt.end,
                });
            }
            return Step::Emit(tt);
        }

        // BINDER (`let!`/`use!`) / AND_BANG (`and!`) — the computation-
        // expression bang binders. FCS handles them with arms that mirror
        // the LET arm exactly (LexFilter.fs:2166-2177): push CtxtLetDecl,
        // let the EQUALS arm push the RHS SeqBlock(AddBlockEnd), and let
        // the CtxtLetDecl offside-pop emit ODECLEND. The raw keyword is
        // consumed (replaced by the virtual at the same span); the parser
        // recovers `let!`/`use!`/`and!` and `isUse` from the raw stream,
        // exactly as it recovers `let`-vs-`use` behind `Virtual::Let`.
        //
        // Two differences from LET: (1) `blockLet` is `CtxtSeqBlock` only —
        // *not* `CtxtMatchClauses` (FCS L2167/2174 omit it); (2) `and!`
        // emits `OAND_BANG` (→ `Virtual::AndBang`), which is a *fresh*
        // CtxtLetDecl, not a let-continuator like plain `and`. There is no
        // `static let!` equivalent (the MemberHead arm above is LET-only),
        // so the bang binders never need that special case.
        if matches!(
            &tt.token,
            TokenContent::Real(Token::LetBang | Token::UseBang | Token::AndBang)
        ) {
            let pos = tt.start;
            let block_let = matches!(self.head(), Some(Context::SeqBlock { .. }));
            let virt = if matches!(&tt.token, TokenContent::Real(Token::AndBang)) {
                Virtual::AndBang
            } else {
                Virtual::Binder
            };
            self.push_ctxt(tt.span.clone(), Context::LetDecl { block_let, pos });
            if block_let {
                return Step::Emit(TokenTup {
                    token: TokenContent::Virtual(virt),
                    span: tt.span,
                    start: tt.start,
                    end: tt.end,
                });
            }
            return Step::Emit(tt);
        }

        // Multi-member pop cascade (LexFilter.fs:2179-2195). A member-
        // start keyword (VAL/STATIC/ABSTRACT/MEMBER/OVERRIDE/DEFAULT)
        // arriving while a `CtxtMemberBody` is on the stack signals
        // the next member declaration: pop everything down to and
        // including the MemberBody, inserting END-tokens for any
        // intermediate contexts that demand one (per
        // `end_token_for_a_ctxt`), then re-feed the keyword. The
        // helper `there_is_a_ctxt_member_body_on_the_stack_and_we_
        // should_pop` matches FCS's guard at L1510-1521 — bail out
        // when the upcoming member is inside an object expression
        // (LBRACE on stack) or inside a static inline constraint
        // (>=2 LPAREN on stack), since those member keywords belong
        // to a nested member, not the enclosing one.
        if matches!(
            &tt.token,
            TokenContent::Real(
                Token::Val
                    | Token::Static
                    | Token::Abstract
                    | Token::Member
                    | Token::Override
                    | Token::Default
            )
        ) && self.there_is_a_ctxt_member_body_on_the_stack_and_we_should_pop()
        {
            // FCS uses `pool.UseLocation(tokenTup, tok)` for each
            // inserted END (LexFilter.fs:2190) — the synthetic borrows
            // the upcoming keyword's location. Snapshot before delaying.
            // The saved keyword and every synthetic END use
            // `delayTokenNoProcessing` (LexFilter.fs:2182, 2190) so they
            // bypass dispatch when popped — they're recovery tokens
            // pre-computed against a known stack and must not re-fire
            // offside rules.
            let loc_span = tt.span.clone();
            let loc_start = tt.start;
            let loc_end = tt.end;
            self.delay_token_no_processing(tt);
            while !matches!(self.head(), Some(Context::MemberBody { .. })) {
                let end_tok = self.head().and_then(|c| self.end_token_for_a_ctxt(c));
                self.pop_ctxt();
                if let Some(v) = end_tok {
                    self.delay_token_no_processing(TokenTup {
                        token: TokenContent::Virtual(v),
                        span: loc_span.clone(),
                        start: loc_start,
                        end: loc_end,
                    });
                }
            }
            self.pop_ctxt(); // pop the MemberBody itself
            return Step::Restart;
        }

        // VAL/STATIC/ABSTRACT/MEMBER/OVERRIDE/DEFAULT push CtxtMemberHead
        // unless the head is already a MemberHead (FCS L2203-2206). The
        // pop-cascade arm above has already handled the case where a
        // MemberBody sat on the stack; whatever survived is the
        // surrounding TypeDefns / ModuleBody / etc.
        if matches!(
            &tt.token,
            TokenContent::Real(
                Token::Val
                    | Token::Static
                    | Token::Abstract
                    | Token::Member
                    | Token::Override
                    | Token::Default
            )
        ) && !matches!(self.head(), Some(Context::MemberHead { .. }))
        {
            self.push_ctxt(tt.span.clone(), Context::MemberHead { pos: tt.start });
            return Step::Emit(tt);
        }

        // PUBLIC/PRIVATE/INTERNAL + lookahead NEW → push CtxtMemberHead
        // (LexFilter.fs:2208-2212). The access modifier introduces a
        // constructor (`public new(...)`); the lookahead disambiguates
        // it from access-modified bindings (`public let x = ...`) which
        // route through the generic LET arm above.
        if matches!(
            &tt.token,
            TokenContent::Real(Token::Public | Token::Private | Token::Internal)
        ) && self
            .peek_next_token_tup()
            .is_some_and(|la| matches!(la.token, TokenContent::Real(Token::New)))
        {
            self.push_ctxt(tt.span.clone(), Context::MemberHead { pos: tt.start });
            return Step::Emit(tt);
        }

        // NEW + lookahead LPAREN → push CtxtMemberHead when the head
        // isn't already one (FCS L2214-2218). The LPAREN check
        // distinguishes a constructor declaration (`new(x, y) = ...`)
        // from an object-creation expression (`new T(x)`); the
        // already-MemberHead guard prevents double-pushing when the
        // PUBLIC/PRIVATE/INTERNAL arm above has already opened the
        // head for `public new(...)`.
        if matches!(&tt.token, TokenContent::Real(Token::New))
            && self
                .peek_next_token_tup()
                .is_some_and(|la| matches!(la.token, TokenContent::Real(Token::LParen)))
            && !matches!(self.head(), Some(Context::MemberHead { .. }))
        {
            self.push_ctxt(tt.span.clone(), Context::MemberHead { pos: tt.start });
            return Step::Emit(tt);
        }
        Step::Pass(tt)
    }

    /// EQUALS-driven body-block pushes across the let/type/with/record/member shapes.
    pub(super) fn equals_pushes(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // EQUALS + CtxtLetDecl head → push SeqBlock(AddBlockEnd), emit
        // EQUALS. (LexFilter.fs:2221)
        if matches!(&tt.token, TokenContent::Real(Token::Equals))
            && matches!(self.head(), Some(Context::LetDecl { .. }))
        {
            self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(tt);
        }

        // EQUALS + CtxtTypeDefns head → replace the head with
        // `equals_end: Some(...)` and push SeqBlock(AddBlockEnd) for
        // the type's RHS. (LexFilter.fs:2226-2230)
        //
        // FCS uses `replaceCtxtIgnoreIndent` (pop + `tryPushCtxt false
        // true`) here: the re-push of the *same* context at the *same*
        // anchor must not re-run the indentation check, or a TypeDefns
        // already flagged offside at `type` gets a second, FCS-absent
        // FS0058 anchored at the `=`. Hence `ignore_indent = true`.
        if matches!(&tt.token, TokenContent::Real(Token::Equals))
            && let Some(Context::TypeDefns { pos, .. }) = self.head().cloned()
        {
            self.pop_ctxt();
            self.try_push_ctxt(
                PushStrictness::AlwaysLenient,
                true,
                // Anchored at the dispatch `=` (never EOF); `ignore_indent` also
                // short-circuits the check, so the flag is doubly irrelevant.
                false,
                tt.span.clone(),
                Context::TypeDefns {
                    pos,
                    equals_end: Some(tt.end),
                },
            );
            self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(tt);
        }

        // EQUALS + CtxtWithAsLet head → push inner SeqBlock for the RHS,
        // emit EQUALS. (LexFilter.fs:2253, first alternative — `with = `
        // shape.) Ported alongside the Vanilla(true) arm below since FCS
        // folds them into the same body. AddBlockEnd vs. NoAddBlockEnd
        // follows `isControlFlowOrNotSameLine` so single-line bindings
        // don't open spurious OBLOCKBEGIN/OBLOCKEND pairs.
        if matches!(&tt.token, TokenContent::Real(Token::Equals))
            && matches!(self.head(), Some(Context::WithAsLet { .. }))
        {
            let add_block_end = if self.is_control_flow_or_not_same_line(&tt) {
                AddBlockEnd::Yes
            } else {
                AddBlockEnd::No
            };
            self.push_ctxt_seq_block(&tt, add_block_end);
            return Step::Emit(tt);
        }

        // EQUALS + record-binding shape → push inner SeqBlock for the RHS,
        // emit EQUALS. (LexFilter.fs:2254, second alternative.) Stack
        // shape: `[..., (WithAsLet | Paren(LBRACE | LBRACE_BAR)),
        // SeqBlock, Vanilla{is_long_ident_equals: true}]`. The Vanilla
        // sits on the LHS identifier (the one we recognised at push time
        // as starting `IDENT (DOT IDENT)* EQUALS`); the SeqBlock is the
        // inner one opened either by the brace-shape WITH dispatch (for
        // record updates) or by the LBRACE Paren itself (for record
        // literals). Without this arm a wrapped RHS like
        // `{ r with A =\n    f x }` would not emit OBLOCKBEGIN before
        // `f`, leaving the inner block unscoped.
        //
        // Same `isControlFlowOrNotSameLine` switch as the WithAsLet-head
        // arm above; FCS comments at L2255-2263 explain why single-line
        // record bindings use NoAddBlockEnd (record updates use `;` to
        // terminate bindings, so an OBLOCKBEGIN/OBLOCKEND pair would
        // mis-shape the parse).
        if matches!(&tt.token, TokenContent::Real(Token::Equals))
            && let [
                ..,
                outer,
                Context::SeqBlock { .. },
                Context::Vanilla {
                    is_long_ident_equals: true,
                    ..
                },
            ] = self.offside_stack.as_slice()
            && matches!(
                outer,
                Context::WithAsLet { .. }
                    | Context::Paren {
                        opener: Opener::Brace | Opener::BraceBar,
                        ..
                    }
            )
        {
            let add_block_end = if self.is_control_flow_or_not_same_line(&tt) {
                AddBlockEnd::Yes
            } else {
                AddBlockEnd::No
            };
            self.push_ctxt_seq_block(&tt, add_block_end);
            return Step::Emit(tt);
        }

        // EQUALS + CtxtMemberHead → replace the head with `CtxtMemberBody`
        // at the SAME anchor position, push an inner `SeqBlock(AddBlockEnd)`
        // for the member's RHS, emit EQUALS. (LexFilter.fs:2271-2275.) FCS
        // uses `replaceCtxt` here — the body inherits the head's column so
        // the offside-pop arm above closes the body when subsequent
        // declarations align with (or left of) the member keyword.
        if matches!(&tt.token, TokenContent::Real(Token::Equals))
            && let Some(Context::MemberHead { pos }) = self.head().cloned()
        {
            self.pop_ctxt();
            self.push_ctxt(tt.span.clone(), Context::MemberBody { pos });
            self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(tt);
        }
        Step::Pass(tt)
    }

    /// for/while/fun/function/exception/->/do/then/else/if/match context pushes.
    pub(super) fn expr_keyword_pushes(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // LAZY / ASSERT → when the operand is on a different line, or is a
        // control-flow keyword, push CtxtSeqBlock(AddBlockEnd) so the whole
        // indented operand block (infix continuations, sequenced statements) is
        // scoped as one `declExpr`, and relabel the keyword to `OLAZY`/`OASSERT`
        // (`Virtual::Lazy`/`Virtual::Assert`) — matching FCS
        // (LexFilter.fs:2232-2237). The parser re-emits the keyword text from the
        // virtual and reads the delayed `BlockBegin` as the "operand is a block"
        // signal (see `parse_lazy_or_assert`). The same-line, non-control-flow
        // case (`lazy a`) pushes nothing and keeps the raw `Token::Lazy`, so its
        // operand is parsed inline, tight (`lazy a + b` = `(lazy a) + b`).
        if matches!(&tt.token, TokenContent::Real(Token::Lazy | Token::Assert))
            && self.is_control_flow_or_not_same_line(&tt)
        {
            let virt = match &tt.token {
                TokenContent::Real(Token::Assert) => Virtual::Assert,
                _ => Virtual::Lazy,
            };
            self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(virt),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }

        // LARROW (`<-`) → the r.h.s. of an assignment begins a new offside
        // block when it is on a different line or starts with a control-flow
        // keyword: push CtxtSeqBlock(AddBlockEnd) so the indented RHS is
        // scoped, then pass `<-` through. (LexFilter.fs:2318-2321.)
        // Context-independent — unlike the EQUALS arms above (gated on the
        // stack head), FCS gates this only on the token plus
        // `isControlFlowOrNotSameLine`. A same-line, non-control-flow RHS
        // (`x <- 1`) pushes nothing and is parsed inline.
        if matches!(&tt.token, TokenContent::Real(Token::LArrow))
            && self.is_control_flow_or_not_same_line(&tt)
        {
            self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(tt);
        }

        // FOR → push CtxtFor, pass FOR through unchanged.
        // (LexFilter.fs:2516)
        if let TokenContent::Real(Token::For) = &tt.token {
            let pos = tt.start;
            let depth = self.paren_depth;
            self.push_ctxt(tt.span.clone(), Context::For { pos, depth });
            return Step::Emit(tt);
        }

        // WHILE → push CtxtWhile, pass WHILE through unchanged.
        // (LexFilter.fs:2521)
        if let TokenContent::Real(Token::While) = &tt.token {
            let pos = tt.start;
            let depth = self.paren_depth;
            self.push_ctxt(tt.span.clone(), Context::While { pos, depth });
            return Step::Emit(tt);
        }

        // FUN → push CtxtFun, emit OFUN. (LexFilter.fs:2532)
        if let TokenContent::Real(Token::Fun) = &tt.token {
            let pos = tt.start;
            let depth = self.paren_depth;
            self.push_ctxt(tt.span.clone(), Context::Fun { pos, depth });
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::Fun),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }

        // FUNCTION → push CtxtFunction + CtxtMatchClauses(leadingBar,
        // lookahead.start), emit OFUNCTION. (LexFilter.fs:2469-2475)
        //
        // Unlike FUN, FUNCTION pushes *two* contexts atomically: the
        // outer CtxtFunction anchors at the `function` keyword and is
        // silent on close (endTokenForACtxt returns None at L1545; the
        // offside arm at L2068 does popCtxt + reprocess). The inner
        // CtxtMatchClauses anchors at the lookahead and emits OEND when
        // it pops, mirroring the MATCH/WITH path.
        //
        // Both pushes are unconditional in FCS (plain `pushCtxt`, not
        // `tryPushCtxt`), and there is no EOF guard on the lookahead:
        // on an incomplete `let f = function` the MatchClauses is
        // pushed at the EOF position so the EOF closure cascade emits
        // the OffsideEnd FCS produces for it.
        //
        // `leading_bar` is true iff the next real token is `|` — this
        // shifts the SemiSemi/`|` column gate in the MatchClauses arm
        // so leading-bar clauses align correctly.
        if let TokenContent::Real(Token::Function) = &tt.token {
            let pos = tt.start;
            self.push_ctxt(tt.span.clone(), Context::Function { pos });
            if let Some(la) = self.peek_next_token_tup() {
                let leading_bar = matches!(la.token, TokenContent::Real(Token::Bar));
                let mc_pos = la.start;
                // Non-strict push (never aborts, matching FCS's `pushCtxt`), but
                // the `CtxtMatchClauses` is anchored at the *lookahead*, which on
                // an incomplete `let f = function` is EOF. FCS reads EOF as
                // column −1 (`startPosOfTokenTup`), so the clauses context is
                // offside of the enclosing `let` and FCS reports FS0058 at EOF.
                // Route through `try_push_ctxt` (not `push_ctxt`) so the EOF
                // anchor feeds `is_correct_indent`'s −1 adjustment.
                let anchor_is_eof = matches!(la.token, TokenContent::Eof);
                self.try_push_ctxt(
                    PushStrictness::AlwaysLenient,
                    false,
                    anchor_is_eof,
                    la.span.clone(),
                    Context::MatchClauses {
                        leading_bar,
                        pos: mc_pos,
                    },
                );
            }
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::Function),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }

        // EXCEPTION → push CtxtException, pass token through unchanged.
        // (LexFilter.fs:2135-2141) FCS guards on `_ :: _` (non-empty
        // stack); after `peek_initial` our stack always has at least
        // the top-level SeqBlock, so the guard is structurally satisfied
        // and elided here. FCS also calls `checkForInvalidDeclsInTypeDefn`
        // for the FS0058 nested-exception diagnostic (no token effect).
        //
        // The context itself is silent on close (endTokenForACtxt = None,
        // L1545); its observable role is to route the subsequent `WITH`
        // through the L2362 dispatch arm rather than the L2462 catch-all.
        if let TokenContent::Real(Token::Exception) = &tt.token {
            self.check_invalid_decl_in_type_defn(NestedDeclKeyword::Exception, &tt);
            self.push_ctxt(tt.span.clone(), Context::Exception { pos: tt.start });
            return Step::Emit(tt);
        }

        // RARROW → push SeqBlock(OneSided) when inside a fun/for/while/
        // when/match-clauses body. The one-sided block has no opening
        // OBLOCKBEGIN; only a closing ORIGHT_BLOCK_END. (LexFilter.fs:2304)
        //
        // Gate on `paren_depth == depth_at_push` for Fun/For/While:
        // `(fun x -> x)` fires (depth stays at the enclosing-paren level)
        // while `fun (g: int -> int) -> …` does not fire on the annotation
        // arrow (depth is one higher than at push). The same gating logic
        // covers `[ for x in xs -> x ]`: at the comprehension arrow,
        // `paren_depth` matches the `for`'s push-time depth (the enclosing
        // `[` raised both equally), so the arm fires; the closing `]`
        // then force-closes SeqBlock(OneSided) + CtxtFor via TokenRExprParen.
        //
        // CtxtWhen/CtxtMatchClauses fire unconditionally — FCS's arm
        // (LexFilter.fs:2308) has no depth gate for them. Also covered:
        // the `CtxtSeqBlock :: CtxtMatchClauses :: _` shape, which arises
        // when a match-arm pattern is itself a sequence-block expression
        // (e.g. an infix-introduced inner block).
        let rarrow_should_push = match &tt.token {
            TokenContent::Real(Token::RArrow) => match self.offside_stack.as_slice() {
                [.., Context::Fun { depth, .. }]
                | [.., Context::For { depth, .. }]
                | [.., Context::While { depth, .. }] => self.paren_depth == *depth,
                [.., Context::MatchClauses { .. }] | [.., Context::When { .. }] => true,
                [
                    ..,
                    Context::MatchClauses { .. } | Context::When { .. },
                    Context::SeqBlock { .. },
                ] => true,
                _ => false,
            },
            _ => false,
        };
        if rarrow_should_push {
            self.push_ctxt_seq_block(&tt, AddBlockEnd::OneSided);
            return Step::Emit(tt);
        }

        // DO / DO_BANG → push CtxtDo + (maybe) SeqBlock(AddBlockEnd) for
        // the body. FCS's arm matches `(DO | DO_BANG)` (LexFilter.fs:2324)
        // and dispatches between `ODO` and `ODO_BANG` on the input token.
        //
        // FCS uses `tryPushCtxtSeqBlock` here (LexFilter.fs:2327): the
        // `try` variant declines to push (and emits no OBLOCKBEGIN) when
        // the body lookahead would be offside or is EOF. Without that,
        // `let f () = do\n` opens a spurious SeqBlock at the EOF
        // position and emits an extra OBLOCKBEGIN/OBLOCKEND pair.
        //
        // Known gap: CtxtDo also expects a `DONE+CtxtDo` balancing arm
        // (LexFilter.fs:1689 — pops CtxtDo, queues ODECLEND at `done`'s
        // range, swallows the DONE) and the `isForLoopContinuator`/
        // `isWhileBlockContinuator` cases that bump pop guards. The
        // current `for ... do ... done` diff tests still pass because
        // those flows pre-date this arm, but a fully `done`-terminated
        // `do`-block as a standalone statement may need the continuator
        // tweak.
        if let TokenContent::Real(Token::Do | Token::DoBang) = &tt.token {
            let virt = match &tt.token {
                TokenContent::Real(Token::DoBang) => Virtual::DoBang,
                _ => Virtual::Do,
            };
            let pos = tt.start;
            self.push_ctxt(tt.span.clone(), Context::Do { pos });
            self.try_push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(virt),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }

        // THEN → push CtxtThen + SeqBlock(AddBlockEnd) for the body,
        // emit OTHEN. (LexFilter.fs:2477)
        if let TokenContent::Real(Token::Then) = &tt.token {
            let pos = tt.start;
            self.push_ctxt(tt.span.clone(), Context::Then { pos });
            self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::Then),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }

        // ELSE → either ELIF rewrite or OELSE + body block.
        // (LexFilter.fs:2483-2504)
        //
        // ELSE IF on the same line is rewritten to a single ELIF: FCS
        // consumes the IF, pushes `CtxtIf` at the ELSE's start position,
        // and returns ELIF with a span covering both tokens. Without
        // this, `if … else if …` desugars into OELSE + OBLOCKBEGIN + IF
        // and the chain's final `else` mis-attaches to the inner if.
        //
        // Otherwise: push CtxtElse + SeqBlock(AddBlockEnd) for the body,
        // emit OELSE. The `then`-body SeqBlock + CtxtThen above on the
        // stack are popped by `tokenForcesHeadContextClosure` (ELSE
        // balances CtxtIf deeper) before we get here.
        if let TokenContent::Real(Token::Else) = &tt.token {
            let pos = tt.start;
            let next = self.peek_next_token_tup();
            if let Some(next) = next
                && matches!(next.token, TokenContent::Real(Token::If))
                && next.start.line == tt.start.line
            {
                let if_end_byte = next.span.end;
                let if_end_pos = next.end;
                self.pop_next_token_tup();
                self.push_ctxt(tt.span.clone(), Context::If { pos });
                return Step::Emit(TokenTup {
                    token: TokenContent::Real(Token::Elif),
                    span: tt.span.start..if_end_byte,
                    start: tt.start,
                    end: if_end_pos,
                });
            }
            self.push_ctxt(tt.span.clone(), Context::Else { pos });
            self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::Else),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }

        // IF / ELIF → push CtxtIf, pass token through unchanged.
        // (LexFilter.fs:2506)
        //
        // Skipped: `isIfBlockContinuator` (THEN/ELSE/ELIF/END/RPAREN +
        // virtual reprocessed tokens). Our minimal `if c then 1 else 2`
        // diff only relies on the basic `tokenStartCol <= offsidePos`
        // guard via the (still-elided) CtxtIf offside-pop, and on the
        // ELSE/ELIF balance arm. Multi-line `if`/`then`/`else` aligned
        // at the same column will force the continuator predicate +
        // CtxtIf offside-pop in.
        if matches!(&tt.token, TokenContent::Real(Token::If | Token::Elif)) {
            let pos = tt.start;
            self.push_ctxt(tt.span.clone(), Context::If { pos });
            return Step::Emit(tt);
        }

        // MATCH / MATCH_BANG → push CtxtMatch, pass through unchanged.
        // (LexFilter.fs:2511) The dedicated WITH arm below transitions
        // CtxtMatch to CtxtMatchClauses when the `with` keyword arrives.
        if matches!(
            &tt.token,
            TokenContent::Real(Token::Match | Token::MatchBang)
        ) {
            let pos = tt.start;
            self.push_ctxt(tt.span.clone(), Context::Match { pos });
            return Step::Emit(tt);
        }
        Step::Pass(tt)
    }

    /// WITH dispatch: the match/try clause head, the host-context and brace-shape
    /// bindings, and the catch-all augment.
    pub(super) fn with_dispatch(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // WITH + (CtxtTry | CtxtMatch) head → push CtxtMatchClauses(
        // leadingBar, lookahead-col), emit OWITH. (LexFilter.fs:2347) The
        // lookahead's start position anchors the new context; `leadingBar`
        // records whether the lookahead is `|` so the CtxtMatchClauses
        // offside-pop can shift its guard accordingly.
        //
        // FCS uses `tryPushCtxt strictIndentation false …` (LexFilter.fs:
        // 2353); we thread the same resolved `strict_indentation_is_error`
        // boolean so that below F# 8 the clauses context is kept (with a
        // warning) instead of aborted, matching FCS's version-dependent tree.
        //
        // The lookahead is *not* EOF-guarded (FCS L2353 pushes with the EOF
        // lookahead too). On an incomplete `match x with\n` / `try x with\n`,
        // `la` is the synthetic EOF, which FCS reads as column −1
        // (`startPosOfTokenTup`) — offside of the enclosing `match`/`try`, so the
        // push runs `is_correct_indent`, which emits FS0058. At F# 8+ (the
        // default) the push is strict and aborts *after* emitting, so no
        // `CtxtMatchClauses` lands and no spurious OEND appears on the EOF
        // cascade — the same stack outcome the old `!Eof` guard produced, now
        // with the diagnostic FCS reports. Below F# 8 it warns and keeps the
        // context, which the EOF cascade then closes with an OEND, exactly as
        // FCS does at 7.0. (Arbitrary-offside *non*-EOF lookahead — `let f x =
        // match x with\nlet g …` — is likewise handled by the strict abort:
        // the second `let` is reprocessed at the outer SeqBlock.)
        //
        // CtxtTry is covered by the same arm: for `try x with _ -> 0` the
        // inner SeqBlock(OneSided) for the try-body has already been
        // force-closed (WITH balances CtxtTry → suffix-exists-balances
        // fires; the SeqBlock at head doesn't balance → it's popped),
        // leaving CtxtTry at head.
        if let TokenContent::Real(Token::With) = &tt.token
            && matches!(
                self.head(),
                Some(Context::Match { .. } | Context::Try { .. })
            )
        {
            if let Some(la) = self.peek_next_token_tup() {
                let leading_bar = matches!(la.token, TokenContent::Real(Token::Bar));
                let pos = la.start;
                let anchor_is_eof = matches!(la.token, TokenContent::Eof);
                self.try_push_ctxt(
                    PushStrictness::VersionGated,
                    false,
                    anchor_is_eof,
                    la.span.clone(),
                    Context::MatchClauses { leading_bar, pos },
                );
            }
            // `with` is consumed (replaced by OWITH) — advance
            // `last_real_end` so any subsequent virtual whose span is
            // computed by `insert_token_from_prev_to_current` (e.g. an
            // OBLOCKSEP fired after a refused MatchClauses push) anchors
            // at `with`'s end rather than the prior real token. FCS's
            // per-token `LastTokenPos` advances unconditionally; we mirror
            // that whenever a real token is swallowed in favour of a
            // virtual. (See similar IN/DONE updates at the LetDecl/Do
            // pop arms.)
            self.last_real_end = tt.span.end;
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::With),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }

        // WITH + (host-context | brace-shape) head — the unified L2362
        // dispatch (LexFilter.fs:2362-2461). Two patterns share one body:
        //
        //   * Host-context: stack ends with one of
        //     `CtxtTypeDefns | CtxtMemberHead | CtxtMemberBody |
        //     CtxtException | CtxtInterfaceHead`.
        //   * Brace-shape: stack ends with
        //     `[.., Paren(LBRACE | LBRACE_BAR), SeqBlock]` — an inner
        //     `with` inside a record-update / anon-record / object-
        //     expression body.
        //
        // In both cases `limCtxt` is the *outer* context whose StartPos
        // is the anchor for any pushed CtxtWithAsLet (multi-line shapes)
        // or CtxtWithAsAugment fallback. The body of the arm dispatches
        // on the lookahead:
        //
        //   * `RBRACE | IDENT | PUBLIC | PRIVATE | INTERNAL | INLINE`
        //     (binding-head class — record-update field, anon-record
        //     field, or property accessor with access modifier) →
        //     push `CtxtWithAsLet(offside)`, optionally an inner
        //     `SeqBlock(NoAddBlockEnd)` if the binding is a long-ident
        //     equals, emit OWITH.
        //   * Same-line `LBRACK_LESS` → property accessor with
        //     attributes (`member x.P with [<Foo>] get() = …`): push
        //     `CtxtWithAsLet(with.start)`, emit OWITH.
        //   * `CtxtInterfaceHead` recovery (LexFilter.fs:2436): when
        //     limCtxt is InterfaceHead AND the lookahead col is at or
        //     left of InterfaceHead's col → emit raw WITH without
        //     pushing anything. The next token participates in the
        //     surrounding SeqBlock as a sibling.
        //   * Otherwise → push `CtxtWithAsAugment(limCtxt.StartPos)` +
        //     `pushCtxtSeqBlock AddBlockEnd`, return raw WITH.
        //
        // `offsidePos` for the binding-head class mirrors FCS L2381-2401:
        // single-line lookahead (col > with.end col) anchors at WITH;
        // otherwise the limCtxt anchors so multi-line bindings align
        // with the record/type column.
        if let TokenContent::Real(Token::With) = &tt.token {
            let (lim_pos, lim_is_interface_head): (Option<Pos>, bool) =
                match self.offside_stack.as_slice() {
                    [
                        ..,
                        Context::TypeDefns { pos, .. }
                        | Context::MemberHead { pos }
                        | Context::MemberBody { pos }
                        | Context::Exception { pos },
                    ] => (Some(*pos), false),
                    [.., Context::InterfaceHead { pos }] => (Some(*pos), true),
                    [
                        ..,
                        Context::Paren {
                            opener: Opener::Brace | Opener::BraceBar,
                            ..
                        },
                        Context::SeqBlock { pos, .. },
                    ] => (Some(*pos), false),
                    _ => (None, false),
                };

            if let Some(lim_pos) = lim_pos {
                let with_end_col = tt.end.col;
                let with_start = tt.start;
                let with_start_line = tt.start.line;
                let la = self.peek_next_token_tup();

                let is_binding_head_class = matches!(
                    la.as_ref().map(|t| &t.token),
                    Some(TokenContent::Real(
                        Token::RBrace
                            | Token::Ident(_)
                            | Token::QuotedIdent(_)
                            | Token::Public
                            | Token::Private
                            | Token::Internal
                            | Token::Inline
                    ))
                );

                if is_binding_head_class {
                    let la_start_col = la.as_ref().unwrap().start.col;
                    let offside_pos = if la_start_col > with_end_col {
                        with_start
                    } else {
                        lim_pos
                    };
                    self.push_ctxt(tt.span.clone(), Context::WithAsLet { pos: offside_pos });

                    // FCS L2414-2421: peek the candidate token, ask
                    // `isLongIdentEquals` whether the upcoming stream forms
                    // `IDENT (DOT IDENT)* EQUALS`, and if so open an inner
                    // SeqBlock(NoAddBlockEnd) so each binding's RHS gets its
                    // own offside scope.
                    let is_followed_by_long_ident_equals =
                        if let Some(candidate) = self.pop_next_token_tup() {
                            let res = match &candidate.token {
                                TokenContent::Real(tok) => self.is_long_ident_equals(tok),
                                _ => false,
                            };
                            self.delay_token(candidate);
                            res
                        } else {
                            false
                        };
                    if is_followed_by_long_ident_equals {
                        self.push_ctxt_seq_block(&tt, AddBlockEnd::No);
                    }

                    self.last_real_end = tt.span.end;
                    return Step::Emit(TokenTup {
                        token: TokenContent::Virtual(Virtual::With),
                        span: tt.span,
                        start: tt.start,
                        end: tt.end,
                    });
                }

                // Else branch: not a binding-head lookahead.

                // FCS L2425-2429: `with [<Foo>] get() = …` — same-line
                // LBRACK_LESS attribute lookahead pushes CtxtWithAsLet
                // anchored at WITH (single-line shape).
                let is_lbrack_less_same_line = la
                    .as_ref()
                    .map(|t| {
                        matches!(t.token, TokenContent::Real(Token::LBrackLess))
                            && t.start.line == with_start_line
                    })
                    .unwrap_or(false);
                if is_lbrack_less_same_line {
                    self.push_ctxt(tt.span.clone(), Context::WithAsLet { pos: tt.start });
                    self.last_real_end = tt.span.end;
                    return Step::Emit(TokenTup {
                        token: TokenContent::Virtual(Virtual::With),
                        span: tt.span,
                        start: tt.start,
                        end: tt.end,
                    });
                }

                // FCS L2432-2434: CtxtInterfaceHead recovery — when
                // `limCtxt` is InterfaceHead AND the lookahead's column
                // sits at or left of the head's column, the `with`
                // body has nothing further indented (typical
                // `interface I with` recovery shape). Emit raw WITH
                // without pushing anything; the next token then
                // participates in the surrounding SeqBlock as a
                // sibling rather than being treated as an augment
                // body element.
                if lim_is_interface_head
                    && let Some(la_tok) = la.as_ref()
                    && la_tok.start.col <= lim_pos.col
                {
                    return Step::Emit(tt);
                }

                // Default else (FCS L2444-2459): push CtxtWithAsAugment
                // anchored at the host's StartPos, then a SeqBlock
                // (AddBlockEnd) for the body. Return raw WITH.
                self.push_ctxt(tt.span.clone(), Context::WithAsAugment { pos: lim_pos });
                self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
                return Step::Emit(tt);
            }
        }

        // WITH catch-all (LexFilter.fs:2462-2466). Reached when none of
        // the host-context or brace-shape patterns above matched —
        // typically `with` at file scope, or `with` whose host context
        // has already been force-closed by upstream balancing. FCS
        // pushes a `CtxtWithAsAugment` anchored at `with` itself and
        // tries (no fallback) to open a body SeqBlock; the raw WITH is
        // returned.
        if let TokenContent::Real(Token::With) = &tt.token {
            self.push_ctxt(tt.span.clone(), Context::WithAsAugment { pos: tt.start });
            self.try_push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(tt);
        }
        Step::Pass(tt)
    }

    /// FINALLY / WHEN / TRY context pushes.
    pub(super) fn try_finally_when_pushes(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // FINALLY + CtxtTry head → push SeqBlock(AddBlockEnd), pass
        // FINALLY through unchanged. (LexFilter.fs:2357) The try-body
        // SeqBlock has already been force-closed by the balance machinery
        // before we reach here. The new SeqBlock holds the finalizer-
        // expression scope; FCS uses `AddBlockEnd` (not OneSided) because
        // the finalizer is a full block expression terminated by an outer
        // offside-pop.
        if let TokenContent::Real(Token::Finally) = &tt.token
            && matches!(self.head(), Some(Context::Try { .. }))
        {
            self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
            return Step::Emit(tt);
        }

        // WHEN + CtxtSeqBlock head → push CtxtWhen, pass WHEN through.
        // (LexFilter.fs:2526) FCS specifically guards on CtxtSeqBlock; in
        // practice this fires inside a match arm where the pattern's
        // RHS has opened a SeqBlock (e.g. an infix-introduced inner
        // block). For bare `| pat when guard -> body`, FCS doesn't push
        // a SeqBlock for the pattern, so this arm is dormant on simple
        // single-line clauses — but our diff tests must pin the rule.
        if let TokenContent::Real(Token::When) = &tt.token
            && matches!(self.head(), Some(Context::SeqBlock { .. }))
        {
            let pos = tt.start;
            self.push_ctxt(tt.span.clone(), Context::When { pos });
            return Step::Emit(tt);
        }

        // TRY → push CtxtTry, then push an inner SeqBlock(OneSided) for the
        // try-body. (LexFilter.fs:2589) FCS uses AddOneSidedBlockEnd
        // because WITH can't be balanced against TRY at the OBLOCKBEGIN-
        // emit time (the lookahead is too costly to disambiguate the many
        // shapes of WITH), so the body block is one-sided: closing
        // ORIGHT_BLOCK_END at the WITH/FINALLY, no opening OBLOCKBEGIN.
        // CtxtTry itself pops silently on offside (no virtual token —
        // `endTokenForACtxt` returns None for it).
        if let TokenContent::Real(Token::Try) = &tt.token {
            let pos = tt.start;
            self.push_ctxt(tt.span.clone(), Context::Try { pos });
            self.push_ctxt_seq_block(&tt, AddBlockEnd::OneSided);
            return Step::Emit(tt);
        }
        Step::Pass(tt)
    }

    /// STRUCT and INTERFACE (paren-form + member-form) pushes.
    pub(super) fn struct_interface_pushes(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        // STRUCT, GUARDED on `CtxtSeqBlock :: (CtxtModuleBody |
        // CtxtTypeDefns) :: _` (LexFilter.fs:2291-2302) so that the
        // typar constraint `<'a when 'a : struct>` does NOT push a
        // CtxtParen — only the body shapes `type T = struct ... end`
        // and `module M = struct ... end` do. Inner SeqBlock is
        // NoAddBlockEnd. Must precede the generic TokenLExprParen
        // handler below so unguarded STRUCT (typar constraint) falls
        // through to that block's `_ => None` arm.
        if let TokenContent::Real(Token::Struct) = &tt.token
            && matches!(
                self.offside_stack.as_slice(),
                [
                    ..,
                    Context::ModuleBody { .. } | Context::TypeDefns { .. },
                    Context::SeqBlock { .. }
                ]
            )
        {
            self.push_ctxt(
                tt.span.clone(),
                Context::Paren {
                    pos: tt.start,
                    opener: Opener::Struct,
                },
            );
            self.push_ctxt_seq_block(&tt, AddBlockEnd::No);
            self.paren_depth += 1;
            return Step::Emit(tt);
        }

        // INTERFACE, GUARDED (LexFilter.fs:2537-2564) on
        // `CtxtSeqBlock :: CtxtTypeDefns(typePos, Some equalsEndPos) :: _`
        // AND INTERFACE is the first real token after `=` AND a
        // lookahead-constrained next token. Inner SeqBlock is AddBlockEnd
        // (unlike STRUCT). Falls through to the L2567 catch-all below
        // for other shapes (CtxtInterfaceHead push).
        //
        // FCS uses `tokenTup.LastTokenPos = equalsEndPos` to check that
        // INTERFACE is the next non-trivia token after `=`. Equivalent
        // here: the outer SeqBlock's `pos` (anchored at the first real
        // token after `=` via `pushCtxtSeqBlock`'s peek) equals
        // INTERFACE's `start`. If the previous-real-token wasn't `=`,
        // the SeqBlock anchored at that other token instead.
        //
        // `allow_deindent` mirrors FCS L2546: when INTERFACE ends on
        // the same line as `=`, the body may indent at any column
        // (limitPos = typePos = column 0); when INTERFACE is on its
        // own line, the body must indent strictly more than INTERFACE
        // itself.
        if let TokenContent::Real(Token::Interface) = &tt.token
            && let [
                ..,
                Context::TypeDefns {
                    pos: type_pos,
                    equals_end: Some(equals_end_pos),
                },
                Context::SeqBlock { pos: seq_pos, .. },
            ] = self.offside_stack.as_slice()
            && *seq_pos == tt.start
        {
            let allow_deindent = tt.end.line == equals_end_pos.line;
            let limit_col = if allow_deindent {
                type_pos.col
            } else {
                tt.start.col
            };
            let type_pos_col = type_pos.col;
            let push = if let Some(la) = self.peek_next_token_tup() {
                match &la.token {
                    TokenContent::Real(Token::End) => la.start.col >= type_pos_col,
                    TokenContent::Real(
                        Token::Default
                        | Token::Override
                        | Token::Interface
                        | Token::New
                        | Token::Type
                        | Token::Static
                        | Token::Member
                        | Token::Abstract
                        | Token::Inherit
                        | Token::LBrackLess,
                    ) => la.start.col > limit_col,
                    _ => false,
                }
            } else {
                false
            };
            if push {
                self.push_ctxt(
                    tt.span.clone(),
                    Context::Paren {
                        pos: tt.start,
                        opener: Opener::Interface,
                    },
                );
                self.push_ctxt_seq_block(&tt, AddBlockEnd::Yes);
                self.paren_depth += 1;
                return Step::Emit(tt);
            }
        }

        // INTERFACE catch-all (LexFilter.fs:2567-2570): any INTERFACE
        // that didn't match the paren-form guard above is a member-
        // style interface implementation (`interface I with …` inside
        // a class body or `type C with` augmentation). Push
        // CtxtInterfaceHead anchored at the keyword and rewrite the
        // token to OINTERFACE_MEMBER → `OffsideInterfaceMember`.
        if let TokenContent::Real(Token::Interface) = &tt.token {
            self.push_ctxt(tt.span.clone(), Context::InterfaceHead { pos: tt.start });
            self.last_real_end = tt.span.end;
            return Step::Emit(TokenTup {
                token: TokenContent::Virtual(Virtual::InterfaceMember),
                span: tt.span,
                start: tt.start,
                end: tt.end,
            });
        }
        Step::Pass(tt)
    }

    /// FCS LexFilter.fs:2330 — *"the r.h.s. of an infix token begins a new
    /// block"*. When an infix operator ([`Self::token_is_infix`]) is followed by
    /// its right-hand side on a **different** line, push a fresh `SeqBlock` at
    /// the rhs position (`AddBlockEnd::No`, no `OBLOCKBEGIN`) so the continuation
    /// line does not start a new statement — i.e. no `OffsideBlockSep` is later
    /// inserted before it. This is what makes
    ///
    /// ```fsharp
    /// let x = a +
    ///         b
    /// ```
    ///
    /// parse as the single expression `a + b` rather than two offside-aligned
    /// statements. The leading-infix form (`a⏎ |> f`) is already handled by the
    /// continuator rule ([`Self::is_seq_block_element_continuator`]); this is the
    /// *trailing*-infix companion.
    ///
    /// Excludes a `MatchClauses` head (FCS's `CtxtMatchClauses :: _` guard): an
    /// infix inside a `when` guard must not open a block for the arm body —
    /// `| _ when a &&⏎ b -> body` keeps `body` in the clause, not a fresh block.
    pub(super) fn infix_rhs_pushes(&mut self, tt: TokenTup<'a>) -> Step<'a> {
        if Self::token_is_infix(&tt.token)
            && !matches!(self.head(), Some(Context::MatchClauses { .. }))
            && self.infix_rhs_on_next_line(&tt)
        {
            self.push_ctxt_seq_block(&tt, AddBlockEnd::No);
            return Step::Emit(tt);
        }
        Step::Pass(tt)
    }

    /// `true` when the token after `tt` (the dispatch infix operator) begins on a
    /// later source line — FCS's `not (isSameLine())` (LexFilter.fs:2332). A
    /// missing / exhausted lookahead counts as "not same line" so a dangling
    /// trailing operator (`a +` at end of input) still opens the recovery block.
    fn infix_rhs_on_next_line(&mut self, tt: &TokenTup<'a>) -> bool {
        match self.peek_next_token_tup() {
            Some(next) => next.start.line != tt.start.line,
            None => true,
        }
    }
}
