//! Token-stream cursor and green-tree emission primitives: the low-level
//! lookahead/`peek`, raw-stream draining for losslessness, text emission, and
//! the `bump_*` / `next_non_trivia_*` helpers the productions build on.

use super::*;

impl<'src> Parser<'src> {
    pub(super) fn new(
        source: &'src str,
        raw_tokens: Vec<RawTok<'src>>,
        filtered_tokens: Vec<FilteredTok<'src>>,
    ) -> Self {
        Self {
            source,
            raw_tokens,
            raw_pos: 0,
            filtered_tokens,
            pos: 0,
            raw_consumed_end: 0,
            builder: GreenNodeBuilder::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            interp_nest: Vec::new(),
            obj_brace_base_new: None,
            obj_brace_base_no_arg: false,
            depth: 0,
            depth_limit_hit: false,
            depth_limit_span: None,
        }
    }

    /// Run `body` one recursion level deeper, bounding the hand-written
    /// recursive descent. The increment/decrement are paired here in one place
    /// so no production can leak depth. Past [`MAX_PARSE_DEPTH`] the body is
    /// *not* run; instead the breach is recorded once and the remaining input is
    /// drained to EOF ([`Self::trigger_depth_limit`]) so every production's
    /// token loop terminates as it would at end-of-input. Once latched, every
    /// guarded entry returns immediately — the unwinding parse does no further
    /// descent.
    ///
    /// Used at the expression / type / pattern recursion chokepoints
    /// (`parse_pratt_expr`, `parse_type`, `climb_pat_tail`). The shared counter
    /// bounds their *combined* depth, which is what stack safety requires (the
    /// three are mutually recursive via `(e : T)`, lambda parameters, match
    /// arms). See [`Self::with_depth_bool`] for the `bool`-returning variant.
    pub(super) fn with_depth(&mut self, body: impl FnOnce(&mut Self)) {
        if self.depth_limit_hit {
            return;
        }
        self.depth += 1;
        if self.depth <= MAX_PARSE_DEPTH {
            body(self);
        } else {
            self.trigger_depth_limit();
        }
        self.depth -= 1;
    }

    /// [`Self::with_depth`] for a chokepoint that returns whether it consumed
    /// anything (`try_emit_atomic_pat`). On the limit — or once latched — it
    /// returns `false`, which terminates the `while self.try_emit_atomic_pat()`
    /// element loops (they also see EOF after the drain, so this is belt and
    /// braces).
    pub(super) fn with_depth_bool(&mut self, body: impl FnOnce(&mut Self) -> bool) -> bool {
        if self.depth_limit_hit {
            return false;
        }
        self.depth += 1;
        let result = if self.depth <= MAX_PARSE_DEPTH {
            body(self)
        } else {
            self.trigger_depth_limit();
            false
        };
        self.depth -= 1;
        result
    }

    /// Record the depth-limit breach (latch + capture the breach span) and drain
    /// the unparsed remainder to EOF as one ERROR node. Draining is what
    /// guarantees termination: the productions' loops (`while let Some(..) =
    /// peek()`, `while !at_closer() && consume_one_seps_group(..)`, the
    /// positive-token `while at_X()` loops) all stop at end-of-input — the same
    /// invariant the parser already relies on for truncated input — so the
    /// unwinding parse runs to completion instead of spinning on a latched
    /// no-op chokepoint.
    fn trigger_depth_limit(&mut self) {
        self.depth_limit_hit = true;
        self.depth_limit_span = Some(
            self.peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len()),
        );
        // Emit every remaining source byte as a single ERROR leaf so
        // `text(tree) == source` still holds, then advance both cursors to the
        // end so no production loop can make further progress.
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ERROR));
        let start = self.raw_consumed_end;
        if start < self.source.len() {
            self.builder.token(
                FSharpLang::kind_to_raw(SyntaxKind::ERROR),
                &self.source[start..],
            );
            self.raw_consumed_end = self.source.len();
        }
        self.builder.finish_node();
        self.raw_pos = self.raw_tokens.len();
        self.pos = self.filtered_tokens.len();
    }

    pub(super) fn peek(&self) -> Option<&FilteredTok<'src>> {
        self.filtered_tokens.get(self.pos)
    }

    /// Drain raw tokens whose span ends at-or-before `up_to`, emitting each
    /// into the green tree. Active trivia and the directive / inactive-code
    /// markers go in at their natural kind; a real token LexFilter rewrote to
    /// a `Virtual` (so the productions never see it) becomes ERROR for
    /// lossless text *and* a `ParseError`. An active lex failure
    /// (`PreprocError::Lex`) becomes ERROR likewise. Structural directive
    /// errors (`UnmatchedEndIf`, `OrphanElse`, …) are filtered out of the raw
    /// stream upstream in [`parse_with_symbols`]; the `Err(_) => None` arm
    /// below is a defensive fallback that would skip one (their bytes are
    /// already covered by the directive's trivia token, and the LSP lexer
    /// producer owns the diagnostic).
    pub(super) fn drain_raw_up_to(&mut self, up_to: usize) {
        while let Some((res, span)) = self.raw_tokens.get(self.raw_pos).cloned() {
            if span.end > up_to {
                break;
            }
            let emit = match res {
                Ok(TriviaToken::Lexed(tok)) => match trivia_kind(&tok) {
                    Some(k) => Some((k, None)),
                    None => Some((
                        SyntaxKind::ERROR,
                        Some(format!(
                            "unsupported token {tok:?} rewritten by LexFilter; Phase 1 does not \
                             parse the surrounding construct"
                        )),
                    )),
                },
                // Directive / inactive-code trivia: never in the filtered
                // stream, so this raw drain is the only path into the tree.
                Ok(marker) => Some((
                    marker
                        .trivia_syntax_kind()
                        .expect("non-Lexed marker is trivia"),
                    None,
                )),
                Err(PreprocError::Lex(e)) => {
                    Some((SyntaxKind::ERROR, Some(format!("lex error: {e:?}"))))
                }
                // Structural directive error — bytes already covered by the
                // directive trivia token; skip (advance only).
                Err(_) => None,
            };
            if let Some((kind, error)) = emit {
                if let Some(message) = error {
                    self.errors.push(ParseError {
                        message,
                        span: span.clone(),
                    });
                }
                self.emit_text(kind, span);
            }
            self.raw_pos += 1;
        }
    }

    /// Emit a green-tree token whose text is the source slice `span`, clamped
    /// at `raw_consumed_end` so the overlapping portion of an FCS-faithful
    /// split (see `raw_consumed_end` doc) isn't re-emitted.
    pub(super) fn emit_text(&mut self, kind: SyntaxKind, span: Range<usize>) {
        let text_start = span.start.max(self.raw_consumed_end);
        let text = if text_start >= span.end {
            ""
        } else {
            &self.source[text_start..span.end]
        };
        self.raw_consumed_end = self.raw_consumed_end.max(span.end);
        self.builder.token(FSharpLang::kind_to_raw(kind), text);
    }

    /// Consume one filtered-stream entry, emitting a green-tree token whose
    /// kind is supplied by the caller. Drains any preceding raw trivia
    /// first.
    ///
    /// Raw-cursor alignment handles three distinct relationships between a
    /// filtered token and the raw token at `raw_pos`:
    ///
    /// * **1:1** (the common case). Filtered span equals raw span — advance
    ///   `raw_pos` past it.
    /// * **Split** (e.g. LexFilter's `typars_close_op_split` slicing one
    ///   `Op(">>=")` into multiple `Greater` + `Equals` filtered tokens).
    ///   The filtered span is a sub-range of the current raw span. Don't
    ///   advance until the *final* sub-piece, identified by
    ///   `raw.end <= filtered.end`.
    /// * **Virtual** (LexFilter insertion carrying the *next real token's*
    ///   span, see `Filter::insert_token`). No raw token is consumed; the
    ///   virtual lands as a zero-width green-tree token so its bytes
    ///   aren't double-counted when the next Raw filtered token follows.
    pub(super) fn bump_into(&mut self, kind: SyntaxKind) {
        let Some((res, span)) = self.filtered_tokens.get(self.pos).cloned() else {
            return;
        };
        let is_virtual = matches!(&res, Ok(FilteredToken::Virtual(_)));

        // Virtuals are inserted *just before* the next real filtered token.
        // For LF (`42\n43`) BlockSep is at [3..3) and draining to span.start
        // (3) flushes the Newline at [2..3) — exactly the trivia the virtual
        // logically follows. For CRLF (`42\r\n43`) BlockSep is at [3..4)
        // (the `\n` byte inside the `\r\n` raw token), and draining only to
        // span.start (3) leaves the `\r\n` un-emitted and stamps the
        // zero-width virtual at offset 2 — before the newline in the tree
        // even though it lives logically *after* it in source. Drain
        // instead up to the next filtered token's start: that's where the
        // virtual conceptually inserts, and any raw whose span fully
        // precedes that point is trivia the virtual logically follows.
        let drain_to = if is_virtual {
            self.filtered_tokens
                .get(self.pos + 1)
                .map(|(_, next)| next.start)
                .unwrap_or(usize::MAX)
        } else {
            span.start
        };
        self.drain_raw_up_to(drain_to);

        self.pos += 1;
        // For Raw filtered tokens, advance `raw_pos` past every raw token the
        // filtered span fully covers (`raw.end <= span.end`). This is:
        // * **1:1** — one raw, equal spans: advances once.
        // * **split** (e.g. `typars_close_op_split`) — earlier pieces have
        //   `raw.end > span.end` so the loop doesn't fire; the final piece
        //   (`raw.end <= span.end`) advances once.
        // * **merge** (`sign_fold` folding `±` + literal into one filtered
        //   token) — the span covers *both* underlying raw tokens (the sign
        //   op and the digits, adjacent so no trivia between), so the loop
        //   advances twice. The next raw token starts at/after `span.end`, so
        //   the loop stops there.
        if !is_virtual {
            while let Some((_, raw_span)) = self.raw_tokens.get(self.raw_pos)
                && raw_span.end <= span.end
            {
                self.raw_pos += 1;
            }
        }
        if is_virtual {
            self.builder.token(FSharpLang::kind_to_raw(kind), "");
        } else {
            self.emit_text(kind, span);
        }
    }

    /// Emit a `ParseError` + zero-width ERROR placeholder for a missing
    /// prefix-operator operand. Shared recovery for `parse_minus_expr`,
    /// `parse_address_of`, `parse_prefix_op_app`,
    /// `parse_address_of_atomic`, and `parse_arg_expr`'s Op arm — each
    /// peeks before recursing into its operand parser; when the operand
    /// position is empty or non-startable, this records the error and
    /// emits an ERROR node so the lossless invariant holds and the
    /// surrounding `APP_EXPR` / `ADDRESS_OF_EXPR` still closes cleanly.
    pub(super) fn push_missing_operand_error(&mut self) {
        self.push_missing_operand_error_with("expected operand after prefix operator");
    }

    /// As [`Self::push_missing_operand_error`] but with a caller-supplied
    /// message — for recovery sites whose missing-operand position isn't a
    /// prefix operator (e.g. the `_.` accessor-function shorthand with no body
    /// expression, FCS's `UNDERSCORE DOT recover`). Records the error at the
    /// current token (or end-of-source) and emits the same zero-width ERROR
    /// placeholder, so the enclosing node still closes losslessly.
    pub(super) fn push_missing_operand_error_with(&mut self, message: &str) {
        let span = self
            .peek()
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| self.source.len()..self.source.len());
        self.errors.push(ParseError {
            message: message.to_string(),
            span,
        });
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ERROR));
        self.builder.finish_node();
    }

    /// First non-trivia *filtered* token strictly after the current
    /// position (`self.pos`). Unlike the raw lookahead helpers this
    /// surfaces layout virtuals (`Virtual::BlockSep`, offside closes, …):
    /// they live in the filtered stream but not the raw stream, so a
    /// raw-only lookahead skips straight over them. The function-form
    /// promotion check in [`Parser::try_emit_head_binding_pat_element`]
    /// pairs this with [`Parser::next_non_trivia_raw_after`] so an
    /// intervening layout virtual — e.g. the element separator between
    /// offside-laid-out list-pattern elements (`[ x⏎ y ]`) — blocks
    /// promotion, while the raw lookahead still rejects a
    /// LexFilter-swallowed `)`. Lex errors count as stoppers (`None`).
    pub(super) fn next_non_trivia_filtered_after_pos(&self) -> Option<&FilteredToken<'src>> {
        self.next_non_trivia_filtered_after_index(self.pos)
    }

    /// As [`Self::next_non_trivia_filtered_after_pos`] but scanning the
    /// filtered stream after an arbitrary index `idx` rather than the cursor.
    /// Used by the parenthesised-operator head lookahead
    /// ([`Parser::paren_op_head_has_args`]), whose head spans *two* filtered
    /// tokens (`(` then the operator), so the "what follows the head" probe
    /// must start past `idx = self.pos + 1`, not the cursor.
    pub(super) fn next_non_trivia_filtered_after_index(
        &self,
        idx: usize,
    ) -> Option<&FilteredToken<'src>> {
        for (res, _) in self.filtered_tokens.iter().skip(idx + 1) {
            match res {
                Ok(FilteredToken::Raw(t)) if trivia_kind(t).is_some() => continue,
                Ok(ft) => return Some(ft),
                Err(_) => return None,
            }
        }
        None
    }

    /// The *index* of the first non-trivia filtered token strictly after `idx`
    /// (a lex error stops the scan, like the token-returning variants). Used by
    /// the access-modifier gate to test an operator-name head after the modifier
    /// (`let private (+) … `), where the head spans an `( op )` pair so the
    /// `at_paren_op_value_pat` probe needs the head's *position*, not just the
    /// token kind.
    pub(super) fn next_non_trivia_filtered_index_after(&self, idx: usize) -> Option<usize> {
        for (i, (res, _)) in self.filtered_tokens.iter().enumerate().skip(idx + 1) {
            match res {
                Ok(FilteredToken::Raw(t)) if trivia_kind(t).is_some() => continue,
                Ok(_) => return Some(i),
                Err(_) => return None,
            }
        }
        None
    }

    /// First non-trivia raw token whose span starts at-or-after `byte`.
    /// Used by `peek_is_expr_start` to look across the LexFilter-swallowed
    /// `RParen` (which never reaches the filtered stream) when deciding
    /// whether `( … )` is a unit literal. Lex errors count as non-trivia
    /// stoppers and produce `None` (not the next ok-token past them):
    /// otherwise `( <bad> )` would commit to unit and then
    /// `bump_swallowed_rparen` would hit its `unreachable!` on the
    /// `Err` token it didn't expect.
    pub(super) fn next_non_trivia_raw_after(&self, byte: usize) -> Option<&Token<'src>> {
        // Raw spans are sorted and contiguous (the full-trivia stream covers
        // every byte in order), so binary-search to the first token starting
        // at-or-after `byte` rather than rescanning from index 0. The callers
        // are hot (the per-operator continuation gates, head-classification
        // lookaheads) and `byte` advances monotonically through the file, so a
        // from-zero skip is O(position) per call — O(n²) over a large file.
        let start = self
            .raw_tokens
            .partition_point(|(_, span)| span.start < byte);
        for (res, _) in &self.raw_tokens[start..] {
            match res {
                Ok(tt) => match raw_significant(tt) {
                    Some(t) => return Some(t),
                    None => continue,
                },
                Err(_) => return None,
            }
        }
        None
    }

    /// `true` if the next significant raw token after byte `op_end` is a
    /// LexFilter-swallowed paren/brace closer (`)` / `}`) — i.e. an operator
    /// ending at `op_end` has its right operand stripped from the filtered
    /// stream because the enclosing body ends with that dangling operator
    /// (`(1 +)`, `(a ::)`, `{1 +}`). The operator-admission gates
    /// ([`Parser::peek_infix_continuation`], [`Parser::peek_cons_continuation`],
    /// [`Parser::peek_type_op_continuation`], [`Parser::at_app_continuation`])
    /// use this to *decline* a body-trailing operator: the filtered RHS-start
    /// lookahead peers past the swallowed closer and would otherwise pull the
    /// post-closer token into the body, draining the real `)` / `}` as `ERROR`.
    /// Leaving the operator for enclosing recovery keeps the closer ending the
    /// body and the following token a sibling.
    pub(super) fn op_rhs_is_swallowed_closer(&self, op_end: usize) -> bool {
        matches!(
            self.next_non_trivia_raw_after(op_end),
            Some(Token::RParen | Token::RBrace)
        )
    }

    /// Fold the offside-block virtuals (`OBLOCKBEGIN` `+1`, `OBLOCKEND` `-1`)
    /// of the filtered tokens in `[*from, pos)` into `*depth`, advancing `*from`
    /// to the cursor. Maintained incrementally by the module decl loop (called
    /// once per iteration), `*depth` tracks the net offside-block nesting since
    /// the body's top level: `0` exactly when the cursor sits at that top level —
    /// between decls — and positive when it is still inside a decl's open
    /// offside block (e.g. a type definition whose `CtxtTypeDefns` block a single
    /// `;` does not close, `LexFilter.fs:1806`). This gates the single-`;` top
    /// separator: a `;` is only a separator at depth `0`; deeper it is still
    /// inside the preceding decl's block (where FCS errors). Stepping once per
    /// iteration (rather than rescanning the whole prefix per `;`) keeps that
    /// gate linear over the body. A decl's own `OBLOCKBEGIN` is consumed inside
    /// its production but still advances `pos`, so it is counted here.
    pub(super) fn advance_block_depth(&self, depth: &mut i32, from: &mut usize) {
        while *from < self.pos {
            match &self.filtered_tokens[*from].0 {
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => *depth += 1,
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) => *depth -= 1,
                _ => {}
            }
            *from += 1;
        }
    }

    /// The first significant raw token *after* the `)` that closes a
    /// parenthesised operator-name head whose operator token ends at byte
    /// `op_end`. [`Parser::at_paren_op_value`] has already verified a `)`
    /// follows the operator, so this locates that `)` in the raw stream and
    /// returns the token after it.
    ///
    /// This is the raw-stream analogue of the function-form promotion's
    /// `next_non_trivia_raw_after(head_ident_end)`: the closing `)` is
    /// LexFilter-swallowed, so the *filtered* stream skips straight past it
    /// (and past any further swallowed *enclosing* `)`), which would make a
    /// filtered-only probe see an outer construct's token as if it were an
    /// argument (`((op)) x`). Reading the raw stream surfaces the next real
    /// `)` (not an atomic-pat start), so the args lookahead declines. Lex
    /// errors and a non-`)` significant token (which `at_paren_op_value`
    /// rules out) both stop the scan (`None`).
    pub(super) fn raw_after_paren_op_close(&self, op_end: usize) -> Option<&Token<'src>> {
        // Binary-search to the first raw token at-or-after `op_end` (spans are
        // sorted/contiguous) rather than rescanning from index 0 — see
        // [`Self::next_non_trivia_raw_after`].
        let start = self
            .raw_tokens
            .partition_point(|(_, span)| span.start < op_end);
        for (res, span) in &self.raw_tokens[start..] {
            match res {
                Ok(tt) => match raw_significant(tt) {
                    Some(Token::RParen) => return self.next_non_trivia_raw_after(span.end),
                    Some(_) => return None,
                    None => continue,
                },
                Err(_) => return None,
            }
        }
        None
    }

    /// First non-trivia raw token at-or-after `raw_pos`, accounting
    /// for LexFilter splits that leave the fused raw partially
    /// consumed. The filtered stream may have skipped a
    /// LexFilter-swallowed `)` past `raw_pos`, so peeking the filtered
    /// stream alone can claim the next real token is what would
    /// actually be reached only by walking *over* the closing paren —
    /// which is wrong for hooks that need to know whether the cursor
    /// is still inside the parens.
    ///
    /// Partial-split case: LexFilter slices a single raw `Op("<^")`
    /// (or `Op(">>")`, `Op("</")`, …) into multiple filtered halves
    /// emitted at sub-ranges of the raw span. After the *first* half
    /// is bumped, `raw_pos` still points at the fused raw because
    /// `bump_into` only advances `raw_pos` past a raw whose full span
    /// is covered. A naïve lookahead would either return the stale
    /// fused raw (e.g. `Op("<^")` after consuming the LESS half) or,
    /// if we just skip it, *look past* the pending split tail and
    /// claim the next-but-one source token is the lookahead. Both
    /// are wrong.
    ///
    /// Correct lookahead in this case is the upcoming filtered token's
    /// *inner* raw — the split tail (`Op("^")` for post-LESS `<^`,
    /// `Greater(true)` for post-first-`>` of `>>`). Detect the
    /// partial-split case by comparing the raw's `span.start` against
    /// `raw_consumed_end` (the byte offset up to which source text has
    /// been emitted): if the raw's start is strictly less, a prefix
    /// has already been emitted. Return the filtered peek's raw if it
    /// is a `FilteredToken::Raw` (the normal case); fall back to the
    /// underlying scan if it's a virtual or EOF (so unusual streams
    /// don't break the existing swallowed-`)` lookahead).
    ///
    /// This is distinct from the swallowed-`)` case: a swallowed
    /// `RParen` raw sits *after* `raw_consumed_end` (no filtered token
    /// has covered it), so the partial-split branch doesn't fire and
    /// the raw flows through the scan as before.
    ///
    /// Mirrors the loop in [`Parser::at_tuple_continuation`]; lex
    /// errors count as stoppers (return `None`).
    pub(super) fn next_non_trivia_raw_at_pos(&self) -> Option<&Token<'src>> {
        let consumed = self.raw_consumed_end;
        if let Some((Ok(TriviaToken::Lexed(raw)), raw_span)) = self.raw_tokens.get(self.raw_pos)
            && raw_span.start < consumed
            && trivia_kind(raw).is_none()
            && let Some((Ok(FilteredToken::Raw(t)), _)) = self.peek()
        {
            return Some(t);
        }
        for (res, span) in self.raw_tokens.iter().skip(self.raw_pos) {
            if span.start < consumed {
                continue;
            }
            match res {
                Ok(tt) => match raw_significant(tt) {
                    Some(t) => return Some(t),
                    None => continue,
                },
                Err(_) => return None,
            }
        }
        None
    }

    /// The `n`-th (0-based) significant (non-trivia) raw token at/after the
    /// cursor. A lightweight multi-token lookahead for head classification —
    /// e.g. the `intersectionType` `'T &` (bare typar, intersection head) vs
    /// `'T<int>` (prefix-app, *not* a head) disambiguation in
    /// [`Parser::at_intersection_head`]. A lex error stops the scan, so an `n`
    /// past it (or past end-of-stream) returns `None`.
    ///
    /// Carries the same **partial-split fallback** as
    /// [`Parser::next_non_trivia_raw_at_pos`]: when `raw_pos` points at a fused
    /// raw whose prefix is already emitted (`Op("<^")` after the `<` half is
    /// bumped — the SRTP-typar generic argument `Foo<^T & …>`), the split tail
    /// (`^`) lives in the filtered stream at the cursor, not as a distinct raw
    /// token. Surface it as index 0 and shift the remaining indices onto the
    /// normal raw scan (which skips the partially-consumed fused raw because its
    /// span starts before `raw_consumed_end`). Without this, index 0 would be
    /// the *ident* behind the split tail and a head typar would be missed.
    pub(super) fn nth_significant_raw_at_pos(&self, n: usize) -> Option<&Token<'src>> {
        let consumed = self.raw_consumed_end;
        let mut shift = 0;
        if let Some((Ok(TriviaToken::Lexed(raw)), raw_span)) = self.raw_tokens.get(self.raw_pos)
            && raw_span.start < consumed
            && trivia_kind(raw).is_none()
            && let Some((Ok(FilteredToken::Raw(t)), _)) = self.peek()
        {
            if n == 0 {
                return Some(t);
            }
            shift = 1;
        }
        self.raw_tokens
            .iter()
            .skip(self.raw_pos)
            .filter(move |(_, span)| span.start >= consumed)
            .map_while(|(res, _)| match res {
                Ok(tt) => Some(raw_significant(tt)),
                Err(_) => None,
            })
            .flatten()
            .nth(n - shift)
    }

    /// First non-trivia raw token at-or-after `raw_pos`, paired with its
    /// source span. Same scan as [`Parser::next_non_trivia_raw_at_pos`]
    /// (including the partial-split fallback to filtered peek), but
    /// returns the span so callers can chain a follow-up
    /// [`Parser::next_non_trivia_raw_after`] for two-token lookahead.
    pub(super) fn next_non_trivia_raw_at_pos_with_span(
        &self,
    ) -> Option<(&Token<'src>, Range<usize>)> {
        let consumed = self.raw_consumed_end;
        if let Some((Ok(TriviaToken::Lexed(raw)), raw_span)) = self.raw_tokens.get(self.raw_pos)
            && raw_span.start < consumed
            && trivia_kind(raw).is_none()
            && let Some((Ok(FilteredToken::Raw(t)), span)) = self.peek()
        {
            return Some((t, span.clone()));
        }
        for (res, span) in self.raw_tokens.iter().skip(self.raw_pos) {
            if span.start < consumed {
                continue;
            }
            match res {
                Ok(tt) => match raw_significant(tt) {
                    Some(t) => return Some((t, span.clone())),
                    None => continue,
                },
                Err(_) => return None,
            }
        }
        None
    }

    /// Consume exactly one FCS `seps` / `seps_block` group, returning whether
    /// anything was consumed.
    ///
    /// FCS's separator productions (`pars.fsy:6981` `seps`, `:5767`
    /// `seps_block`) are a *single* group — one of `SEMICOLON`, `OBLOCKSEP`,
    /// `SEMICOLON OBLOCKSEP`, or `OBLOCKSEP SEMICOLON` — i.e. at most one `;`
    /// (raw [`Token::Semi`], emitted [`SyntaxKind::SEMI_TOK`]) with at most one
    /// adjacent offside [`Virtual::BlockSep`] (emitted as a zero-width
    /// [`SyntaxKind::ERROR`], the existing layout-virtual convention). A
    /// *repeated* separator (`A; ; B`) is therefore **not** one group: callers
    /// that loop one group per gap leave the extra separator to trip the next
    /// element parser's recovery, matching FCS (which reports a parse error).
    ///
    /// `at_closer` reports when the enclosing form's closer is the next
    /// significant token. It guards the optional *trailing* `BlockSep` of a
    /// `SEMICOLON OBLOCKSEP` group so a separator belonging to an *enclosing*
    /// scope is not absorbed — this matters when the closer is swallowed (the
    /// record `}` is absent from the filtered stream but still ahead in the raw
    /// stream; see [`Parser::bump_swallowed_closer`]). For a real filtered
    /// closer (`|}`, `]`) the predicate is a plain `peek` check and the guard is
    /// merely defensive.
    pub(super) fn consume_one_seps_group(&mut self, at_closer: impl Fn(&Self) -> bool) -> bool {
        let is_semi = |p: &Self| matches!(p.peek(), Some((Ok(FilteredToken::Raw(Token::Semi)), _)));
        let is_block_sep = |p: &Self| {
            matches!(
                p.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            )
        };
        if is_block_sep(self) {
            self.bump_into(SyntaxKind::ERROR); // OBLOCKSEP
            if !at_closer(self) && is_semi(self) {
                self.bump_into(SyntaxKind::SEMI_TOK); // OBLOCKSEP SEMICOLON
            }
            true
        } else if is_semi(self) {
            self.bump_into(SyntaxKind::SEMI_TOK); // SEMICOLON
            if !at_closer(self) && is_block_sep(self) {
                self.bump_into(SyntaxKind::ERROR); // SEMICOLON OBLOCKSEP
            }
            true
        } else {
            false
        }
    }

    /// `true` if the filtered cursor holds a sign-folded literal (see
    /// [`token_is_folded_signed_literal`]) that the parser will consume next.
    /// The fold ([`super::sign_fold`]) merges `±`+literal in the *filtered*
    /// stream only, so the raw lookahead at the cursor still surfaces the
    /// pre-fold `Op("-")`/`Op("+")`; the pattern-start gates OR this in so the
    /// folded constant is recognised in continuation/nested positions.
    ///
    /// Guards against a LexFilter-swallowed closer between the cursor and the
    /// literal: the raw token at the cursor must *begin where the folded
    /// literal does* (its sign), not at an earlier swallowed `)`/`}`. So in
    /// `(h ::) -1` the `::` rhs still bails (the raw cursor is the `)`,
    /// starting before the `-1`), while `1 :: -1` folds the rhs.
    pub(super) fn folded_signed_literal_at_cursor(&self) -> bool {
        let Some((Ok(FilteredToken::Raw(tok)), fspan)) = self.peek() else {
            return false;
        };
        token_is_folded_signed_literal(tok)
            && self
                .next_non_trivia_raw_at_pos_with_span()
                .is_some_and(|(_, rspan)| rspan.start == fspan.start)
    }

    /// Drain raw trivia, then emit the next non-trivia raw token (expected
    /// to be `Token::RParen`) directly into the green tree under the given
    /// syntax kind, advancing `raw_pos` past it.
    ///
    /// `RParen` is "swallowed" by our lexfilter (it never reaches the
    /// filtered stream — see `peek_is_expr_start` doc), so the parser
    /// can't consume it through the normal `bump_into` path.
    ///
    /// The unit-literal caller (`parse_const_expr`) has already validated
    /// via `peek_is_expr_start` that the next non-trivia raw is `RParen`,
    /// so the recovery path below is unreachable for it. The paren-expr
    /// caller (`parse_paren_expr`) has no such guarantee — the inner
    /// `parse_expr` only consumes one atomic expression today, so inputs
    /// like `(1+2)`, `(f x)`, or an incomplete `(1` (no closing paren)
    /// would leave a non-`RParen` token here. We record a `ParseError`
    /// and return without consuming the offender, leaving the outer
    /// loop to drive recovery. The `PAREN_EXPR` node closes at whatever
    /// extent has been built; the tree is still lossless because trivia
    /// drained before the error was emitted with its real text.
    pub(super) fn bump_swallowed_rparen(&mut self, kind: SyntaxKind) {
        self.bump_swallowed_closer(
            kind,
            |t| matches!(t, Token::RParen),
            ")",
            "parenthesised expression",
        );
    }

    /// Generalised swallowed-closer emitter. LexFilter removes a construct's
    /// closing delimiter from the *filtered* stream in some contexts (the
    /// `)` of a paren expression, the `}` of a computation expression), so a
    /// production that opened such a construct finds the closer only in the
    /// raw stream. Drain leading raw trivia, then emit the closer (matched by
    /// `is_closer`) under `kind`; on a non-closer or EOF, push a recovery
    /// error keyed by `closer` / `construct` for the diagnostic text. The
    /// `)`-specific [`Self::bump_swallowed_rparen`] is the original caller;
    /// [`Self::parse_brace_expr`] reuses it for `}`.
    pub(super) fn bump_swallowed_closer(
        &mut self,
        kind: SyntaxKind,
        is_closer: impl Fn(&Token<'src>) -> bool,
        closer: &str,
        construct: &str,
    ) {
        while let Some((res, span)) = self.raw_tokens.get(self.raw_pos).cloned() {
            if let Ok(tt) = &res
                && let Some(tk) = raw_trivia_kind(tt)
            {
                self.emit_text(tk, span);
                self.raw_pos += 1;
                continue;
            }
            if let Ok(TriviaToken::Lexed(t)) = &res
                && is_closer(t)
            {
                self.emit_text(kind, span);
                self.raw_pos += 1;
                return;
            }
            self.errors.push(ParseError {
                message: format!("expected `{closer}` to close {construct}, found {res:?}"),
                span,
            });
            return;
        }
        let end = self.source.len();
        self.errors.push(ParseError {
            message: format!("unexpected end of input inside {construct}; expected `{closer}`"),
            span: end..end,
        });
    }
}
