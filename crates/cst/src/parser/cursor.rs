//! Token-stream cursor and tree emission primitives: the low-level
//! lookahead/`peek`, raw-stream draining for losslessness, text emission, and
//! the `bump_*` / `next_non_trivia_*` helpers the productions build on.

use super::*;

impl<'src> Parser<'src> {
    pub(super) fn new(
        source: &'src str,
        raw_tokens: RawTokens<'src>,
        filtered_tokens: FilteredTokens<'src>,
    ) -> Self {
        Self {
            source,
            raw_tokens,
            raw_pos: 0,
            filtered: FilteredCursor::new(filtered_tokens),
            raw_consumed_end: 0,
            builder: TreeBuilder::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            interp_nest: Vec::new(),
            obj_brace_base_new: None,
            obj_brace_base_no_arg: false,
            depth: 0,
            depth_limit_hit: false,
            depth_limit_span: None,
            tree_height_stop: TREE_HEIGHT_STOP,
        }
    }

    /// A fresh parser over the same tokens that stops at tree height `stop`
    /// (see [`parse_inner`]); this one's partial tree and diagnostics are
    /// dropped.
    pub(super) fn restart(self, stop: usize) -> Self {
        // The tree being thrown away is too tall by construction, perhaps far
        // taller than any tree a parse returns.
        self.builder.discard();
        Self {
            tree_height_stop: stop,
            ..Self::new(self.source, self.raw_tokens, self.filtered.into_tokens())
        }
    }

    /// Build the whole file as a root holding the source as one `ERROR` token,
    /// with the depth limit latched: the last resort of [`parse_inner`] for a
    /// file whose tree no stop height keeps under [`MAX_TREE_HEIGHT`].
    pub(super) fn give_up_whole(&mut self, file_kind: FileKind) {
        let root = match file_kind {
            FileKind::Impl => SyntaxKind::IMPL_FILE,
            FileKind::Sig => SyntaxKind::SIG_FILE,
        };
        self.builder.start_node(root);
        self.trigger_depth_limit();
        self.builder.finish_node();
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
        if self.depth_limit_latched() {
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
        if self.depth_limit_latched() {
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

    /// Whether the depth limit is latched, latching it first if the tree being
    /// built has reached its stop height (`tree_height_stop`, normally
    /// [`TREE_HEIGHT_STOP`]; see [`MAX_TREE_HEIGHT`]).
    ///
    /// Latching drains the rest of the input, so it may only happen where a
    /// production is ready to see end of input next: at a recursion chokepoint
    /// ([`Self::with_depth`]), or at the head of a loop that wraps one more
    /// node per repeat, before it looks for the next repeat — such a chain
    /// grows the tree without recursing. Never mid-production: a production
    /// that has looked ahead and then bumps a token may rely on the tokens it
    /// saw still following.
    pub(super) fn depth_limit_latched(&mut self) -> bool {
        if !self.depth_limit_hit && self.builder.height_if_closed_now() >= self.tree_height_stop {
            self.trigger_depth_limit();
        }
        self.depth_limit_hit
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
        self.builder.start_node(SyntaxKind::ERROR);
        let start = self.raw_consumed_end;
        if start < self.source.len() {
            self.builder.token(SyntaxKind::ERROR, &self.source[start..]);
            self.raw_consumed_end = self.source.len();
        }
        self.builder.finish_node();
        self.raw_pos = self.raw_tokens.len();
        self.filtered.advance_to_end();
    }

    /// The filtered token at the cursor.
    pub(super) fn peek(&self) -> Option<&FilteredTok<'src>> {
        self.filtered.current()
    }

    /// Drain raw tokens whose span ends at-or-before `up_to`, emitting each
    /// into the tree. Active trivia and the directive / inactive-code
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
        while self.raw_pos < self.raw_tokens.len() {
            let span = self.raw_tokens.span(self.raw_pos);
            if span.end > up_to {
                break;
            }
            // Trivia (active whitespace and comments, and the directive and
            // inactive-code markers, which are never in the filtered stream,
            // so this raw drain is their only path into the tree) is emitted
            // as itself; anything else reaching the drain is an `ERROR`.
            let kind = self.raw_tokens.kind(self.raw_pos);
            let (kind, error) = match kind.trivia_syntax_kind() {
                Some(k) => (k, None),
                None => match self.raw_tokens.get(self.raw_pos).map(|(res, _)| res) {
                    Some(Ok(TriviaToken::Lexed(tok))) => (
                        SyntaxKind::ERROR,
                        Some(format!(
                            "unsupported token {tok:?} rewritten by LexFilter; Phase 1 does not \
                             parse the surrounding construct"
                        )),
                    ),
                    Some(Err(PreprocError::Lex(e))) => {
                        (SyntaxKind::ERROR, Some(format!("lex error: {e:?}")))
                    }
                    other => unreachable!(
                        "a non-trivia table entry is a token or a lex error, not {other:?}"
                    ),
                },
            };
            if let Some(message) = error {
                self.errors.push(ParseError {
                    message,
                    span: span.clone(),
                });
            }
            self.emit_text(kind, span);
            self.raw_pos += 1;
        }
    }

    /// Whether the raw token at `raw_pos` is a significant token whose text the
    /// tree already holds part of: a fused raw token the lex-filter split, of
    /// which an earlier piece has been bumped (see
    /// [`Self::next_non_trivia_raw_at_pos`]).
    fn raw_pos_is_a_split_token(&self) -> bool {
        self.raw_pos < self.raw_tokens.len()
            && self.raw_tokens.span(self.raw_pos).start < self.raw_consumed_end
            && matches!(self.raw_tokens.kind(self.raw_pos), RawKind::Lexed(_))
            && !self.raw_tokens.kind(self.raw_pos).is_trivia()
    }

    /// The first significant raw token at or after index `from` whose span
    /// starts at or after byte `skip_before`, with its span. Trivia is skipped;
    /// a lexer error ends the scan with `None`, as does the end of the stream.
    fn significant_raw_from(
        &self,
        from: usize,
        skip_before: usize,
    ) -> Option<(Token<'src>, Range<usize>)> {
        for i in from..self.raw_tokens.len() {
            let span = self.raw_tokens.span(i);
            if span.start < skip_before || self.raw_tokens.kind(i).is_trivia() {
                continue;
            }
            return self.raw_tokens.token(i).map(|t| (t, span));
        }
        None
    }

    /// Emit a token whose text is the source slice `span`, clamped
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
        self.builder.token(kind, text);
    }

    /// Consume one filtered-stream entry, emitting a token whose
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
    ///   virtual lands as a zero-width token so its bytes
    ///   aren't double-counted when the next Raw filtered token follows.
    ///
    /// A virtual may only be stamped [`SyntaxKind::ERROR`] or its own marker
    /// kind ([`virtual_marker_kind`]), and a real token never a marker kind.
    /// The productions gate on the *raw* stream as well as the filtered one, so
    /// a raw-stream lookahead can approve a `bump_into` while the filtered
    /// cursor is parked on a layout virtual, which would then land as a
    /// zero-width real token. [`stamp_mismatch`] catches that whole class here:
    /// a debug build panics, so the differential suites fail loudly; a release
    /// build stamps `ERROR` and records the internal error as a parse error,
    /// keeping the tree lossless and the file marked broken rather than
    /// handing consumers a zero-width identifier or keyword.
    #[track_caller]
    pub(super) fn bump_into(&mut self, kind: SyntaxKind) {
        let Some((res, span)) = self.filtered.current().cloned() else {
            return;
        };
        let is_virtual = matches!(&res, Ok(FilteredToken::Virtual(_)));
        let kind = match stamp_mismatch(&res, kind) {
            None => kind,
            Some(message) => {
                if cfg!(debug_assertions) {
                    // The caller's location names the production that
                    // approved the bump (`bump_into` is `#[track_caller]`).
                    panic!(
                        "{message}, from {}, at {span:?}",
                        std::panic::Location::caller()
                    );
                }
                self.errors.push(ParseError {
                    message: format!("internal parser error: {message}"),
                    span: span.clone(),
                });
                SyntaxKind::ERROR
            }
        };

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
            self.filtered
                .tokens()
                .span_at(self.filtered.pos() + 1)
                .map_or(usize::MAX, |next| next.start)
        } else {
            span.start
        };
        self.drain_raw_up_to(drain_to);

        self.filtered.advance();
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
            while self.raw_pos < self.raw_tokens.len()
                && self.raw_tokens.span(self.raw_pos).end <= span.end
            {
                self.raw_pos += 1;
            }
        }
        if is_virtual {
            self.builder.token(kind, "");
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
        self.builder.start_node(SyntaxKind::ERROR);
        self.builder.finish_node();
    }

    /// First non-trivia *filtered* token strictly after the current
    /// position (`self.filtered.pos()`). Unlike the raw lookahead helpers this
    /// surfaces layout virtuals (`Virtual::BlockSep`, offside closes, …):
    /// they live in the filtered stream but not the raw stream, so a
    /// raw-only lookahead skips straight over them. The function-form
    /// promotion check in [`Parser::try_emit_head_binding_pat_element`]
    /// pairs this with [`Parser::next_non_trivia_raw_after`] so an
    /// intervening layout virtual — e.g. the element separator between
    /// offside-laid-out list-pattern elements (`[ x⏎ y ]`) — blocks
    /// promotion, while the raw lookahead still rejects a
    /// LexFilter-swallowed `)`. Lex errors count as stoppers (`None`).
    pub(super) fn next_non_trivia_filtered_after_pos(&self) -> Option<FilteredToken<'src>> {
        self.next_non_trivia_filtered_after_index(self.filtered.pos())
    }

    /// As [`Self::next_non_trivia_filtered_after_pos`] but scanning the
    /// filtered stream after an arbitrary index `idx` rather than the cursor.
    /// Used by the parenthesised-operator head lookahead
    /// ([`Parser::operator_head_after`]), whose head spans *two* filtered
    /// tokens (`(` then the operator), so the "what follows the head" probe
    /// must start past `idx = self.filtered.pos() + 1`, not the cursor.
    pub(super) fn next_non_trivia_filtered_after_index(
        &self,
        idx: usize,
    ) -> Option<FilteredToken<'src>> {
        self.filtered
            .tokens()
            .get(self.next_non_trivia_filtered_index_after(idx)?)?
            .0
            .ok()
    }

    /// The *index* of the first non-trivia filtered token strictly after `idx`
    /// (a lex error stops the scan, like the token-returning variants). Used by
    /// the access-modifier gate to test an operator-name head after the modifier
    /// (`let private (+) … `), where the head spans an `( op )` pair so the
    /// `at_paren_op_value_pat` probe needs the head's *position*, not just the
    /// token kind.
    pub(super) fn next_non_trivia_filtered_index_after(&self, idx: usize) -> Option<usize> {
        // The filtered stream holds no trivia (`FilteredKind::of`), so the
        // first non-trivia token after `idx` is the next one.
        let next = idx + 1;
        (next < self.filtered.tokens().len() && !self.filtered.tokens().kind(next).is_error())
            .then_some(next)
    }

    /// First non-trivia raw token whose span starts at-or-after `byte`.
    /// Used by `peek_is_expr_start` to look across the LexFilter-swallowed
    /// `RParen` (which never reaches the filtered stream) when deciding
    /// whether `( … )` is a unit literal. Lex errors count as non-trivia
    /// stoppers and produce `None` (not the next ok-token past them):
    /// otherwise `( <bad> )` would commit to unit and then
    /// `bump_swallowed_rparen` would hit its `unreachable!` on the
    /// `Err` token it didn't expect.
    pub(super) fn next_non_trivia_raw_after(&self, byte: usize) -> Option<Token<'src>> {
        // Raw spans are sorted and contiguous (the full-trivia stream covers
        // every byte in order), so binary-search to the first token starting
        // at-or-after `byte` rather than rescanning from index 0. The callers
        // are hot (the per-operator continuation gates, head-classification
        // lookaheads) and `byte` advances monotonically through the file, so a
        // from-zero skip is O(position) per call — O(n²) over a large file.
        let start = self.raw_tokens.count_starting_before(byte);
        self.significant_raw_from(start, 0).map(|(t, _)| t)
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
        while *from < self.filtered.pos() {
            match self.filtered.tokens().kind(*from) {
                FilteredKind::Virtual(Virtual::BlockBegin) => *depth += 1,
                FilteredKind::Virtual(Virtual::BlockEnd) => *depth -= 1,
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
    pub(super) fn raw_after_paren_op_close(&self, op_end: usize) -> Option<Token<'src>> {
        // Binary-search to the first raw token at-or-after `op_end` (spans are
        // sorted/contiguous) rather than rescanning from index 0 — see
        // [`Self::next_non_trivia_raw_after`].
        let start = self.raw_tokens.count_starting_before(op_end);
        match self.significant_raw_from(start, 0)? {
            (Token::RParen, span) => self.next_non_trivia_raw_after(span.end),
            _ => None,
        }
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
    pub(super) fn next_non_trivia_raw_at_pos(&self) -> Option<Token<'src>> {
        let consumed = self.raw_consumed_end;
        if self.raw_pos_is_a_split_token()
            && let Some((Ok(FilteredToken::Raw(t)), _)) = self.peek()
        {
            return Some(t.clone());
        }
        self.significant_raw_from(self.raw_pos, consumed)
            .map(|(t, _)| t)
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
    pub(super) fn nth_significant_raw_at_pos(&self, n: usize) -> Option<Token<'src>> {
        let consumed = self.raw_consumed_end;
        let mut shift = 0;
        if self.raw_pos_is_a_split_token()
            && let Some((Ok(FilteredToken::Raw(t)), _)) = self.peek()
        {
            if n == 0 {
                return Some(t.clone());
            }
            shift = 1;
        }
        let raw = &self.raw_tokens;
        (self.raw_pos..raw.len())
            .filter(|&i| raw.span(i).start >= consumed)
            .map_while(|i| match raw.kind(i) {
                RawKind::Lexed(_) => Some(i),
                kind if kind.is_trivia() => Some(i),
                _ => None,
            })
            .filter(|&i| !raw.kind(i).is_trivia())
            .nth(n - shift)
            .and_then(|i| raw.token(i))
    }

    /// First non-trivia raw token at-or-after `raw_pos`, paired with its
    /// source span. Same scan as [`Parser::next_non_trivia_raw_at_pos`]
    /// (including the partial-split fallback to filtered peek), but
    /// returns the span so callers can chain a follow-up
    /// [`Parser::next_non_trivia_raw_after`] for two-token lookahead.
    pub(super) fn next_non_trivia_raw_at_pos_with_span(
        &self,
    ) -> Option<(Token<'src>, Range<usize>)> {
        let consumed = self.raw_consumed_end;
        if self.raw_pos_is_a_split_token()
            && let Some((Ok(FilteredToken::Raw(t)), span)) = self.peek()
        {
            return Some((t.clone(), span.clone()));
        }
        self.significant_raw_from(self.raw_pos, consumed)
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
    /// to be `Token::RParen`) directly into the tree under the given
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
        while let Some((res, span)) = self.raw_tokens.get(self.raw_pos) {
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

/// The kind of the zero-width marker token a virtual becomes when the tree
/// keeps it as a marker; `None` for the virtuals the tree records as a
/// zero-width [`SyntaxKind::ERROR`] (the layout virtuals, and the relabels
/// whose backing raw keyword a dedicated emitter takes instead, such as
/// [`Parser::emit_let_keyword`]). Exhaustive, so a new virtual has to choose.
fn virtual_marker_kind(virtual_token: &Virtual) -> Option<SyntaxKind> {
    match virtual_token {
        Virtual::HighPrecedenceParenApp => Some(SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK),
        Virtual::HighPrecedenceBrackApp => Some(SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK),
        Virtual::Let
        | Virtual::Binder
        | Virtual::AndBang
        | Virtual::BlockBegin
        | Virtual::BlockEnd
        | Virtual::DeclEnd
        | Virtual::BlockSep
        | Virtual::Do
        | Virtual::DoBang
        | Virtual::Then
        | Virtual::Else
        | Virtual::Fun
        | Virtual::Function
        | Virtual::Lazy
        | Virtual::Assert
        | Virtual::End
        | Virtual::With
        | Virtual::JoinIn
        | Virtual::InterfaceMember
        | Virtual::RightBlockEnd
        | Virtual::HighPrecedenceTyApp => None,
    }
}

/// What a virtual stands for, relative to the raw stream.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VirtualClass {
    /// A block opener, closer or separator LexFilter inserts with no raw token
    /// behind it.
    Layout,
    /// A relabel of the raw keyword at the same span (`Let`, `Then`, `With`,
    /// …): the raw stream still shows what comes next.
    Relabel,
    /// An adjacency marker just before the raw token it precedes
    /// (`HighPrecedence*`), with no raw token of its own.
    Marker,
}

/// Exhaustive, so a new virtual has to choose.
fn virtual_class(virtual_token: &Virtual) -> VirtualClass {
    match virtual_token {
        Virtual::BlockBegin
        | Virtual::BlockEnd
        | Virtual::DeclEnd
        | Virtual::BlockSep
        | Virtual::End
        | Virtual::RightBlockEnd => VirtualClass::Layout,
        Virtual::Let
        | Virtual::Binder
        | Virtual::AndBang
        | Virtual::Do
        | Virtual::DoBang
        | Virtual::Then
        | Virtual::Else
        | Virtual::Fun
        | Virtual::Function
        | Virtual::Lazy
        | Virtual::Assert
        | Virtual::With
        | Virtual::JoinIn
        | Virtual::InterfaceMember => VirtualClass::Relabel,
        Virtual::HighPrecedenceParenApp
        | Virtual::HighPrecedenceBrackApp
        | Virtual::HighPrecedenceTyApp => VirtualClass::Marker,
    }
}

/// Whether `virtual_token` is pure layout ([`VirtualClass::Layout`]).
fn is_layout_virtual(virtual_token: &Virtual) -> bool {
    virtual_class(virtual_token) == VirtualClass::Layout
}

impl Parser<'_> {
    /// Whether the filtered cursor sits on a layout virtual
    /// ([`is_layout_virtual`]). A production that dispatches on the raw stream
    /// at the cursor must not commit there: the raw token it sees is not the
    /// next one the parser consumes, and bumping it would stamp the virtual
    /// instead (see [`Self::bump_into`]). The raw lookaheads themselves do not
    /// stop at one, since many productions read past a layout virtual on
    /// purpose, to decide whether to consume it.
    pub(super) fn parked_on_layout_virtual(&self) -> bool {
        matches!(self.peek(), Some((Ok(FilteredToken::Virtual(v)), _)) if is_layout_virtual(v))
    }

    /// Whether a filtered token at or after the cursor stands for the raw token
    /// starting at byte `start`: the raw token itself (or the first piece of a
    /// split), or a relabelling virtual ([`VirtualClass::Relabel`]) backed by
    /// it. The consumer of that filtered token advances the raw cursor past the
    /// raw one, so a production must not claim such a raw token from the raw
    /// stream. A layout virtual at the same span stands for nothing: LexFilter
    /// places one at the span of the token it precedes.
    pub(super) fn filtered_stream_carries(&self, start: usize) -> bool {
        self.filtered
            .tokens()
            .items_from(self.filtered.pos())
            .take_while(|(_, span)| span.start <= start)
            .any(|(res, span)| {
                span.start == start
                    && match res {
                        Ok(FilteredToken::Raw(_)) => true,
                        Ok(FilteredToken::Virtual(v)) => virtual_class(&v) == VirtualClass::Relabel,
                        Err(_) => false,
                    }
            })
    }

    /// Whether the filtered cursor meets the next `n` raw tokens with no layout
    /// virtual ([`VirtualClass::Layout`]) before any of them. A production that
    /// approves `n` tokens through a raw-stream lookahead and then bumps them
    /// all needs this too: the raw check proves which tokens they are (and that
    /// no LexFilter-swallowed closer sits between), this proves no dedent or
    /// block separator was inserted in front of one, where the bump would
    /// stamp it instead. A relabelling virtual (`Virtual::Let`, …) stands for
    /// its raw keyword and counts as one of the `n`; an adjacency marker
    /// (`HighPrecedence*`) has no raw token and is passed over.
    pub(super) fn next_tokens_unbroken(&self, n: usize) -> bool {
        let mut met = 0;
        for (res, _) in self.filtered.tokens().items_from(self.filtered.pos()) {
            if met == n {
                return true;
            }
            match res {
                Ok(FilteredToken::Raw(_)) => met += 1,
                Ok(FilteredToken::Virtual(v)) => match virtual_class(&v) {
                    VirtualClass::Layout => return false,
                    VirtualClass::Relabel => met += 1,
                    VirtualClass::Marker => {}
                },
                Err(_) => return false,
            }
        }
        met == n
    }

    /// Whether the filtered cursor reaches the real token starting at byte
    /// `start` with no layout virtual in front of it or of anything before it,
    /// except inside attribute lists (`[< … >]`), whose arguments are
    /// expressions with their own layout. The span-keyed form of
    /// [`Self::next_tokens_unbroken`], for a lookahead whose run may cross an
    /// attribute list (where a LexFilter-swallowed closer makes the raw and
    /// filtered token counts disagree).
    pub(super) fn reaches_unbroken_outside_attributes(&self, start: usize) -> bool {
        let mut in_attribute = false;
        for (res, span) in self.filtered.tokens().items_from(self.filtered.pos()) {
            match res {
                Ok(FilteredToken::Raw(_)) if span.start >= start => return span.start == start,
                Ok(FilteredToken::Raw(Token::LBrackLess)) => in_attribute = true,
                Ok(FilteredToken::Raw(Token::GreaterRBrack)) => in_attribute = false,
                Ok(FilteredToken::Raw(_)) => {}
                Ok(FilteredToken::Virtual(v)) if is_layout_virtual(&v) && !in_attribute => {
                    return false;
                }
                Ok(FilteredToken::Virtual(_)) => {}
                Err(_) => return false,
            }
        }
        false
    }
}

/// Why stamping `kind` onto the filtered entry `entry` would be wrong, or
/// `None` when it is right: a virtual takes only `ERROR` or its own
/// [`virtual_marker_kind`], and a real token never takes a marker kind (it has
/// width; a marker has none). A lex error takes whatever the production chose.
fn stamp_mismatch(entry: &Result<FilteredToken<'_>, LexError>, kind: SyntaxKind) -> Option<String> {
    match entry {
        Ok(FilteredToken::Virtual(v)) => (kind != SyntaxKind::ERROR
            && virtual_marker_kind(v) != Some(kind))
        .then(|| format!("bump_into stamped {kind:?} onto the layout virtual {v:?}")),
        Ok(FilteredToken::Raw(t)) => matches!(
            kind,
            SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK | SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK
        )
        .then(|| format!("bump_into stamped the marker kind {kind:?} onto the real token {t:?}")),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_virtual_takes_only_error_or_its_own_marker_kind() {
        let virtual_entry = |v| Ok(FilteredToken::Virtual(v));
        let block_sep = virtual_entry(Virtual::BlockSep);
        assert_eq!(stamp_mismatch(&block_sep, SyntaxKind::ERROR), None);
        assert!(stamp_mismatch(&block_sep, SyntaxKind::IDENT_TOK).is_some());
        assert!(stamp_mismatch(&block_sep, SyntaxKind::SEMI_TOK).is_some());
        assert!(stamp_mismatch(&block_sep, SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK).is_some());

        let paren_app = virtual_entry(Virtual::HighPrecedenceParenApp);
        assert_eq!(stamp_mismatch(&paren_app, SyntaxKind::ERROR), None);
        assert_eq!(
            stamp_mismatch(&paren_app, SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK),
            None
        );
        assert!(stamp_mismatch(&paren_app, SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK).is_some());

        let brack_app = virtual_entry(Virtual::HighPrecedenceBrackApp);
        assert_eq!(
            stamp_mismatch(&brack_app, SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK),
            None
        );
        assert!(stamp_mismatch(&brack_app, SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK).is_some());
    }

    #[test]
    fn a_real_token_never_takes_a_marker_kind() {
        let ident = Ok(FilteredToken::Raw(Token::Ident("a")));
        assert_eq!(stamp_mismatch(&ident, SyntaxKind::IDENT_TOK), None);
        assert_eq!(stamp_mismatch(&ident, SyntaxKind::ERROR), None);
        assert!(stamp_mismatch(&ident, SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK).is_some());
        assert!(stamp_mismatch(&ident, SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK).is_some());
    }

    /// `a` with an offside block opening at it: the filtered cursor starts on
    /// the zero-width `OBLOCKBEGIN` while the raw cursor already sees the `a`,
    /// which is the misalignment the stamp guard exists for.
    fn parser_parked_on_a_virtual() -> Parser<'static> {
        let mut parser = Parser::new(
            "a",
            lex_full_trivia("a", &HashSet::new()).tokens,
            FilteredTokens::from_items(
                "a",
                [
                    (Ok(FilteredToken::Virtual(Virtual::BlockBegin)), 0..1),
                    (Ok(FilteredToken::Raw(Token::Ident("a"))), 0..1),
                ],
            ),
        );
        parser.builder.start_node(SyntaxKind::IMPL_FILE);
        parser
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "bump_into stamped IDENT_TOK onto the layout virtual BlockBegin")]
    fn stamping_a_real_kind_onto_a_virtual_panics_in_a_debug_build() {
        parser_parked_on_a_virtual().bump_into(SyntaxKind::IDENT_TOK);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn stamping_a_real_kind_onto_a_virtual_is_an_error_in_a_release_build() {
        let mut parser = parser_parked_on_a_virtual();
        parser.bump_into(SyntaxKind::IDENT_TOK);
        parser.bump_into(SyntaxKind::IDENT_TOK);
        parser.builder.finish_node();
        let root = parser.builder.finish();
        let kinds: Vec<_> = root
            .children_with_tokens()
            .map(|t| (t.kind(), t.to_string()))
            .collect();
        assert_eq!(
            kinds,
            [
                (SyntaxKind::ERROR, String::new()),
                (SyntaxKind::IDENT_TOK, "a".to_string())
            ]
        );
        assert_eq!(parser.errors.len(), 1);
        assert!(
            parser.errors[0]
                .message
                .starts_with("internal parser error: bump_into stamped IDENT_TOK")
        );
    }

    #[test]
    fn stamping_error_onto_a_virtual_is_silent() {
        let mut parser = parser_parked_on_a_virtual();
        parser.bump_into(SyntaxKind::ERROR);
        parser.bump_into(SyntaxKind::IDENT_TOK);
        assert!(parser.errors.is_empty());
    }
}
