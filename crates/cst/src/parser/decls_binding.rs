//! Long-identifier paths, attribute lists, and `let` binding productions (the
//! binding LHS; the `= <expr>` RHS lives in [`super::expr`]).

use super::*;

impl<'src> Parser<'src> {
    /// Emit a bare `LONG_IDENT` node — `IDENT (DOT IDENT)*` — for a context
    /// that consumes a dotted path directly rather than wrapped in an
    /// expression / type / pattern: the `open Foo.Bar` target
    /// (`SynOpenDeclTarget.ModuleOrNamespace`'s `SynLongIdent`) and the
    /// phase-8.2 `module Foo` / `namespace Foo.Bar` header names
    /// (`SynModuleOrNamespace.longId`). Mirrors the raw-gated dotted loop in
    /// [`Self::parse_atomic_type`]: each `DOT` extends the path only when a
    /// real ident follows on the *raw* stream, so a trailing dot recovers
    /// without crossing a layout boundary or a LexFilter-swallowed token. An
    /// empty path (e.g. `open\n` or `namespace\n`) records an error and emits
    /// an empty `LONG_IDENT`, matching FCS's `OPEN`/`namespace` recover
    /// (`pars.fsy:1399`/`:560`) which yields an empty path. `context` names
    /// the leading keyword for the "expected identifier after `…`"
    /// diagnostic.
    ///
    /// Returns the number of identifier segments parsed (`Foo.Bar.Baz` → 3, an
    /// empty/erroneous head → 0). The module-abbreviation slice (8.5) uses this
    /// to reject a dotted LHS (`module X.Y = Z`); other callers ignore it.
    pub(super) fn parse_long_ident_path(&mut self, context: &str) -> usize {
        self.parse_long_ident_path_with(context, false)
    }

    /// As [`Self::parse_long_ident_path`], but `allow_underscore_head` accepts a
    /// leading `_` as the first path segment, emitted as an `IDENT_TOK` with
    /// text `"_"`. Used only by the member head ([`Self::parse_member_head_pat`])
    /// for the wildcard self-identifier `member _.M = …`, which FCS stores as a
    /// `SynPat.LongIdent` whose first `Ident.idText` is `"_"`. The general path
    /// callers (`open`/`module`/`namespace`/`type`) pass `false`, where a `_`
    /// head is invalid.
    pub(super) fn parse_long_ident_path_with(
        &mut self,
        context: &str,
        allow_underscore_head: bool,
    ) -> usize {
        // Drain leading trivia (the inter-token whitespace after the
        // `open`/`module`/`namespace` keyword) into the parent node so the
        // `LONG_IDENT` range stays tight around the path, matching
        // `parse_type`'s self-draining convention.
        if let Some((_, span)) = self.peek() {
            let start = span.start;
            self.drain_raw_up_to(start);
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        let mut segments = 0usize;
        // A leading `_` self-identifier (`member _.M`) is accepted only when the
        // caller opts in; FCS treats it as a path segment whose `idText` is
        // `"_"`. Emitted as `IDENT_TOK` (de-quoted to `"_"` by the normaliser,
        // matching FCS).
        let head_is_underscore = allow_underscore_head
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Underscore)), _))
            );
        match self.peek().cloned() {
            // A `_` head (`member _.M`) — accepted only under
            // `allow_underscore_head`. Bumped as `IDENT_TOK` (text `"_"`), then
            // the shared dot-continuation below.
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _)) if head_is_underscore => {
                self.bump_into(SyntaxKind::IDENT_TOK);
                segments += 1;
            }
            // `global` is a valid *leading* path segment (FCS's `path`
            // production: `GLOBAL | …`, `pars.fsy`), so `open global.System`
            // and bare `open global` are legal. FCS stores it as an ident
            // whose `idText` is the backtick-quoted `` `global` `` plus an
            // `IdentTrivia.OriginalNotation "global"`; we emit the raw
            // `global` text as `IDENT_TOK`, and the normaliser (which prefers
            // OriginalNotation on the FCS side, and strips backticks on ours)
            // lines both up to `"global"`. Only valid as the head — the
            // dot-continuation below stays `Ident`/`QuotedIdent`.
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_) | Token::Global)),
                _,
            )) => {
                self.bump_into(SyntaxKind::IDENT_TOK);
                segments += 1;
            }
            other => {
                let span = other
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: format!("expected identifier after `{context}`"),
                    span,
                });
                self.builder.finish_node();
                return segments;
            }
        }
        // Shared dot-continuation (`. ident`)*. Reached once a head segment has
        // been bumped (`_` self-id, `global`, or an ident); each continuation
        // segment stays `Ident`/`QuotedIdent`.
        segments += self.sweep_long_ident_dot_continuation();
        self.builder.finish_node();
        segments
    }

    /// Sweep a `(DOT ident)*` dot-continuation into the *currently open*
    /// `LONG_IDENT` node, after its head segment has already been bumped.
    /// Returns the number of *additional* segments appended (`Foo` already
    /// bumped, then `.Bar.Baz` ⇒ `2`).
    ///
    /// Each `DOT` extends the path only when a real ident follows on the *raw*
    /// stream (`next_non_trivia_raw_at_pos` for the dot, with the filtered
    /// cursor confirmed at it so a layout virtual the raw lookahead skipped
    /// doesn't promote a cross-line `Foo⏎.Bar` into one path). A trailing dot
    /// (`Foo.` with no following ident) records a "trailing dot in long
    /// identifier path" error and stops — the `DOT_TOK` is still emitted, so
    /// the tree stays lossless.
    ///
    /// Shared by [`Self::parse_long_ident_path_with`] (the `open` / `module` /
    /// `namespace` / `type` heads) and the long-ident *pattern* heads
    /// ([`Self::try_emit_atomic_pat`], [`Self::try_emit_head_binding_pat_element`]),
    /// so FCS's `pathOp` is consumed identically wherever a dotted path appears.
    pub(super) fn sweep_long_ident_dot_continuation(&mut self) -> usize {
        let mut segments = 0usize;
        while self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Dot))
        {
            // Confirm the filtered cursor is at the dot, not a layout virtual
            // the raw lookahead skipped past.
            let Some((Ok(FilteredToken::Raw(Token::Dot)), dot_span)) = self.peek().cloned() else {
                break;
            };
            self.bump_into(SyntaxKind::DOT_TOK);
            let raw_next_is_ident = matches!(
                self.next_non_trivia_raw_after(dot_span.end),
                Some(Token::Ident(_) | Token::QuotedIdent(_)),
            );
            match self.peek().cloned() {
                Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), _))
                    if raw_next_is_ident =>
                {
                    self.bump_into(SyntaxKind::IDENT_TOK);
                    segments += 1;
                }
                _ => {
                    self.errors.push(ParseError {
                        message: "trailing dot in long identifier path".to_string(),
                        span: dot_span,
                    });
                    break;
                }
            }
        }
        segments
    }

    /// `pars.fsy:3094 hardwhiteLetBindings` — `OLET opt_rec localBindings
    /// hardwhiteDefnBindingsTerminator`. Phase 4.2 handles `opt_rec` and the
    /// `and`-chained `localBindings` (`localBinding moreLocalBindings`),
    /// still restricted to the single-ident-LHS shape:
    ///
    /// ```text
    /// OLET [REC] IDENT EQUALS OBLOCKBEGIN <expr> [OBLOCKEND]
    ///     (AND IDENT EQUALS OBLOCKBEGIN <expr> [OBLOCKEND])*
    ///     ODECLEND
    /// ```
    ///
    /// Builds `LET_DECL > [LET_TOK, REC_TOK?, BINDING, (AND_TOK, BINDING)*]`
    /// with trivia interleaved. The `OBLOCKBEGIN` virtual that opens each
    /// RHS is consumed as a zero-width ERROR placeholder inside its binding;
    /// each binding's trailing `OBLOCKEND` is consumed inside `LET_DECL` (as
    /// ERROR) only if an `and` continuation follows, so a non-continuing
    /// `OBLOCKEND` plus the final `ODECLEND` still fall through to the
    /// outer `parse_impl_file` loop's virtual-fallthrough arm.
    ///
    /// LexFilter's `is_let_continuator` (LexFilter.fs:336) keeps
    /// `CtxtLetDecl` open across `Token::And` via the `+1` offside guard,
    /// so the raw `and` reaches us in the filtered stream regardless of
    /// whether the preceding `let` had `rec` — FCS accepts the no-`rec`
    /// form too (with warning FS0588) and emits the same single
    /// `SynModuleDecl.Let` with `isRec = false`.
    ///
    /// Caller must have already verified that `peek()` returns
    /// `Virtual::Let`.
    /// Parse one or more *adjacent* `[< … >]` attribute lists (phase 10.5),
    /// emitting an `ATTRIBUTE_LIST` node per group. Adjacent lists form the
    /// carrier's `SynAttributes` (FCS's `attributes: attributeList attributes`
    /// recursion concatenates them); the next-list check uses the *raw* stream
    /// so it sees through an inter-line `BlockSep`. Does *not* open a wrapper
    /// node — the caller wraps the lists onto a carrier (e.g. a `let`-binding's
    /// `LET_DECL`). The caller has verified `peek()` is `Token::LBrackLess`.
    pub(super) fn parse_attribute_lists(&mut self) {
        loop {
            self.parse_attribute_list();
            // Continue to a further list only when the next list is in the *same*
            // scope — reachable across inter-line `BlockSep` separators (and
            // trivia) alone. A scope-closing `BlockEnd` (or any other token) stops
            // the run, so a following list in the *enclosing* scope (e.g.
            // `module A =⏎ [<A>]⏎[<B>] …`, the `[<B>]` outside `A`) is left for the
            // caller rather than folded across `A`'s body close. Decide
            // *without consuming*: a swallowed `type`/`module` keyword or an
            // offside name after the `BlockSep` must keep the `BlockSep` in place
            // for the carrier dispatch's own drain (the 10.7a type-header path).
            let next_is_same_scope_list = self
                .filtered_tokens
                .iter()
                .skip(self.pos)
                .find_map(|(res, _)| match res {
                    Ok(FilteredToken::Raw(t)) if trivia_kind(t).is_some() => None,
                    Ok(FilteredToken::Virtual(Virtual::BlockSep)) => None,
                    other => Some(other),
                })
                .is_some_and(|t| matches!(t, Ok(FilteredToken::Raw(Token::LBrackLess))));
            if !next_is_same_scope_list {
                break;
            }
            // Confirmed: consume the inter-list `BlockSep`(s) as zero-width ERRORs
            // so the next list's `bump_into` lands on the real `[<`.
            while matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            ) {
                self.bump_into(SyntaxKind::ERROR);
            }
        }
    }

    /// Parse one `[< … >]` group into an `ATTRIBUTE_LIST`, holding one or more
    /// `;`-separated attributes (FCS's `attributeListElements: attribute |
    /// attributeListElements seps attribute`, `pars.fsy:1535`). The caller has
    /// verified `peek()` is `Token::LBrackLess`.
    pub(super) fn parse_attribute_list(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ATTRIBUTE_LIST));
        self.bump_into(SyntaxKind::LBRACK_LESS_TOK);
        self.parse_attribute();

        // `;`-separated further attributes (FCS's `attributeListElements:
        // attribute (seps attribute)*`). Each gap is exactly one `seps` group;
        // FCS's `seps` (`pars.fsy:6981`) is one of `;`, `OBLOCKSEP`,
        // `OBLOCKSEP ;`, or `; OBLOCKSEP` — i.e. at most one `;` (raw
        // `Token::Semi` → `SEMI_TOK`) with at most one adjacent offside
        // `Virtual::BlockSep` (zero-width `ERROR`). Inside `[< … >]` a
        // `BlockSep` is emitted only when the continuation aligns at the
        // enclosing block's offside column (e.g. top-level `[<A;\nB>]` /
        // `[<A\nB>]` with `B` at column 0); an indented continuation produces
        // none. Consuming exactly one group per gap matches FCS on both error
        // shapes (verified against `fcs-dump`): a *repeated* separator
        // (`[<A; ; B>]`) leaves the extra to make the next `parse_attribute`
        // raise "expected attribute name", and a no-separator continuation
        // (the indented `[<A\n  B>]`, which emits no `BlockSep`) leaves the
        // stray attribute to trip the `>]` check below. A trailing group before
        // `>]` is tolerated (FCS's `opt_seps`).
        loop {
            let is_semi =
                |p: &Self| matches!(p.peek(), Some((Ok(FilteredToken::Raw(Token::Semi)), _)));
            let is_block_sep = |p: &Self| {
                matches!(
                    p.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                )
            };
            // Consume one `seps` group, or stop if no separator is present.
            if is_block_sep(self) {
                self.bump_into(SyntaxKind::ERROR); // OBLOCKSEP
                if is_semi(self) {
                    self.bump_into(SyntaxKind::SEMI_TOK); // OBLOCKSEP ;
                }
            } else if is_semi(self) {
                self.bump_into(SyntaxKind::SEMI_TOK); // ;
                if is_block_sep(self) {
                    self.bump_into(SyntaxKind::ERROR); // ; OBLOCKSEP
                }
            } else {
                break;
            }
            // A trailing separator group before `>]` carries no further attribute.
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::GreaterRBrack)), _)),
            ) {
                break;
            }
            self.parse_attribute();
        }

        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::GreaterRBrack)), _)),
        ) {
            self.bump_into(SyntaxKind::GREATER_RBRACK_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, span)| span.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `>]` to close attribute list".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// Parse the optional `attributeTarget` prefix of an attribute (phase
    /// 10.5c) — FCS's `attributeTarget` (`pars.fsy:1565`): `ident COLON`,
    /// `typeKeyword COLON` (→ "type"), or `YIELD COLON` (→ "return"). Emits an
    /// `ATTRIBUTE_TARGET > [IDENT_TOK, COLON_TOK]` (the target word always as
    /// `IDENT_TOK`, since its source text is FCS's canonical `Target` idText)
    /// when a target word is immediately followed by `:`; otherwise leaves the
    /// cursor untouched (the common no-target case). The `<head> COLON` shape is
    /// unambiguous against a `path` (`[<Foo.Bar>]` has `.`, not `:`).
    ///
    /// Two flavours, by how the target word reaches us:
    /// * **Filtered head** — an `ident` / quoted-ident (`assembly:`, `field:`,
    ///   …) or the `return` keyword (`Token::Return`, which is *not* swallowed):
    ///   the word is the current filtered token, so a one-token filtered
    ///   lookahead for `:` suffices.
    /// * **Swallowed `type`** — `type` lexes as `Token::Type` and LexFilter
    ///   *swallows* it inside `[< … >]` (it pushes a transient `CtxtTypeDefns`),
    ///   so the filtered cursor is already at the `:` and the keyword survives
    ///   only on the raw stream. Recover it exactly as `open type` does
    ///   ([`Self::parse_open_decl`]): emit the raw `type` span as `IDENT_TOK`,
    ///   advance `raw_pos`, then consume the filtered `:`.
    ///
    /// `module` is deliberately *not* recovered: it is likewise swallowed, but
    /// `[<module: …>]` is an FCS parse error (its `moduleKeyword COLON` arm is
    /// unreachable once the keyword drives the module-head machinery), so it
    /// falls through to `parse_attribute`'s path parser, which errors.
    fn parse_opt_attribute_target(&mut self) {
        let head_is_filtered_target = matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Ident(_) | Token::QuotedIdent(_) | Token::Return
                )),
                _,
            )),
        );
        if head_is_filtered_target
            && matches!(
                self.next_non_trivia_filtered_after_pos(),
                Some(FilteredToken::Raw(Token::Colon)),
            )
        {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::ATTRIBUTE_TARGET));
            self.bump_into(SyntaxKind::IDENT_TOK);
            self.bump_into(SyntaxKind::COLON_TOK);
            self.builder.finish_node();
            return;
        }

        // Swallowed `type:` — the filtered cursor is at the `:`, with the raw
        // `type` keyword sitting just before it on the raw stream.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _)))
            && let Some((Token::Type, type_span)) = self.next_non_trivia_raw_at_pos_with_span()
        {
            let type_span = type_span.clone();
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::ATTRIBUTE_TARGET));
            self.drain_raw_up_to(type_span.start);
            self.emit_text(SyntaxKind::IDENT_TOK, type_span);
            self.raw_pos += 1;
            self.bump_into(SyntaxKind::COLON_TOK);
            self.builder.finish_node();
        }
    }

    /// Parse a single custom attribute into an `ATTRIBUTE` — FCS's
    /// `attribute: attributeTarget? path opt_HIGH_PRECEDENCE_APP
    /// opt_atomicExprAfterType` (`pars.fsy:1542`). The optional `attributeTarget`
    /// prefix (phase 10.5c) is an `ATTRIBUTE_TARGET` node; the `path` is a
    /// `LONG_IDENT`; the optional `atomicExprAfterType` argument (phase 10.5b)
    /// is a trailing `Expr` child. A *bare* attribute (no arg) leaves the
    /// `ArgExpr` as FCS's synthetic `mkSynUnit`, supplied by the normaliser.
    pub(super) fn parse_attribute(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ATTRIBUTE));
        self.parse_opt_attribute_target();
        match self.peek().cloned() {
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_) | Token::Global)),
                _,
            )) => {
                // `path` → `LONG_IDENT > [IDENT_TOK (DOT_TOK IDENT_TOK)*]`.
                // `global` is accepted only as the head segment (FCS's `path`
                // production, mirroring the expr long-ident parser); the raw
                // text is emitted as `IDENT_TOK` and the normaliser lines both
                // sides up to `"global"` via the OriginalNotation trivia. The
                // dot-continuation below stays `Ident`/`QuotedIdent`. Inside
                // `[< … >]` there is no enclosing swallowed `)`, so the dot
                // continuation can gate on the plain filtered peek.
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
                self.bump_into(SyntaxKind::IDENT_TOK);
                while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Dot)), _))) {
                    let dot_span = self.peek().map(|(_, s)| s.clone()).expect("dot peeked");
                    self.bump_into(SyntaxKind::DOT_TOK);
                    match self.peek().cloned() {
                        Some((
                            Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                            _,
                        )) => {
                            self.bump_into(SyntaxKind::IDENT_TOK);
                        }
                        _ => {
                            self.errors.push(ParseError {
                                message: "trailing dot in attribute path".to_string(),
                                span: dot_span,
                            });
                            break;
                        }
                    }
                }
                self.builder.finish_node(); // LONG_IDENT

                // Optional argument expr (phase 10.5b) — FCS's
                // `attribute: path opt_HIGH_PRECEDENCE_APP opt_atomicExprAfterType`.
                // An *adjacent* `(` (`[<Foo(1)>]`) is preceded by LexFilter's
                // `HighPrecedenceParenApp` marker (FCS's `opt_HIGH_PRECEDENCE_APP`);
                // consume it as a zero-width `ERROR` like `parse_app_expr` does,
                // then the `(` is parsed as the argument atom. A spaced
                // (`[<Foo (1)>]`) or non-paren (`[<Foo "x">]`) argument has no
                // marker; both forms produce the same `ArgExpr` (verified).
                if self.peek_is_paren_app_marker() {
                    self.bump_into(SyntaxKind::ERROR);
                }
                // Gate before delegating to `parse_atomic_expr`
                // ([`Self::peek_starts_aftertype_arg`], shared with
                // `parse_new_expr` / `parse_inherit_member`):
                // - `(` (unit / parenExpr): the `parse_atomic_expr`
                //   LParen-dispatch precondition (so a malformed `[<Foo(>]`
                //   leaves the `(` for the `>]` check instead of hitting the
                //   dispatch's `unreachable!`), minus the `( op )` operator-value
                //   which `atomicExprAfterType` excludes (`[<Foo(+)>]` is an FCS
                //   error).
                // - everything else: `raw_starts_attribute_arg` (the
                //   `atomicExprAfterType` starters, excluding bare idents, the
                //   prefix-op forms, and the glued `(*)`), so `[<Foo>]` /
                //   `[<Foo; Bar>]` stay bare.
                let starts_arg = self.peek_starts_aftertype_arg();
                if starts_arg {
                    self.parse_atomic_expr();
                }
            }
            other => {
                let span = other
                    .map(|(_, span)| span)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected attribute name".to_string(),
                    span,
                });
            }
        }
        self.builder.finish_node(); // ATTRIBUTE
    }

    /// Parse a `let`/`use` declaration into a `node` of the caller's choosing —
    /// [`SyntaxKind::LET_DECL`] for a module-level `let`, or
    /// [`SyntaxKind::MEMBER_LET_BINDINGS`] for a class-local `let` in an
    /// object-model body (phase 9.8b); the internal shape (`[LET_TOK, REC_TOK?,
    /// BINDING, (AND_TOK BINDING)*]`) is identical (FCS uses the same
    /// `localBindings` grammar for both). With `cp = None` this is the plain
    /// form; with `cp = Some(checkpoint)` the caller has already emitted one or
    /// more leading `ATTRIBUTE_LIST`s (phase 10.5) after the checkpoint, and this
    /// wraps them together with the binding so the attributes are leading
    /// children before `LET_TOK`.
    pub(super) fn parse_let_decl_at(&mut self, cp: Option<rowan::Checkpoint>, node: SyntaxKind) {
        // Use the let token's span (which carries the rewritten real token's
        // range) to place the leading trivia, same convention as
        // `parse_module_decl`.
        let let_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_let_decl_at invoked without a peeked let token");
        match cp {
            // Attributed form: the `ATTRIBUTE_LIST`s sit between the checkpoint
            // and here; wrap them with the binding. The whitespace between the
            // closing `>]` and `let` belongs *inside* the decl (after the
            // attributes), so drain it once the node is open.
            Some(cp) => {
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(node));
                self.drain_raw_up_to(let_span.start);
            }
            // Plain form: leading trivia/comments stay siblings of the decl.
            None => {
                self.drain_raw_up_to(let_span.start);
                self.builder.start_node(FSharpLang::kind_to_raw(node));
            }
        }
        // Head keyword (`LET_TOK`), optional `REC_TOK`, first binding (FCS's
        // `localBinding`), and any `and`-chained followers (`moreLocalBindings`),
        // with FCS error 576 for a non-recursive chain. Shared with the
        // expression-level `parse_let_or_use_expr` and the module let-in
        // dispatch `parse_module_let`. The helper claims the let keyword whether
        // it arrives as `Virtual::Let` (LexFilter's `OffsideLet` rewrite) or as
        // a raw `Token::Let`/`Token::Use` (a `let` after an attribute list or a
        // `;` on the same line).
        self.parse_let_head_and_bindings();

        self.builder.finish_node(); // LET_DECL / MEMBER_LET_BINDINGS
    }

    /// Parse an `extern` DllImport prototype (FCS's `cPrototype`,
    /// `pars.fsy:3186`) into an [`SyntaxKind::EXTERN_DECL`]: `extern cRetType
    /// opt_access ident ( externArgs )`. FCS lowers this to a
    /// `SynModuleDecl.Let([binding])` with `SynLeadingKeyword.Extern`, a
    /// `LongIdent(name, Pats[Tuple[…]])` head pattern of C-typed arguments, and a
    /// synthetic `failwith "…"` RHS; the normaliser projects our `EXTERN_DECL` to
    /// that same shape.
    ///
    /// Handles plain-path C types (`int`, `System.IntPtr`, a bare `byref` path,
    /// …), `void`, and recursive C-type suffix forms (`T&` / `T*` / `T[]` /
    /// `void*` / `T*[]` / ...).
    /// With `cp = Some(_)` the caller has already emitted leading
    /// `ATTRIBUTE_LIST`s (the `[<DllImport(…)>]` carrier) after the checkpoint,
    /// wrapped here into the `EXTERN_DECL` (FCS's `SynBinding.attributes`). The
    /// caller has verified the cursor is at the raw `extern` keyword.
    pub(super) fn parse_extern_decl_at(&mut self, cp: Option<rowan::Checkpoint>) {
        let extern_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_extern_decl_at invoked without a peeked extern keyword");
        match cp {
            Some(cp) => {
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::EXTERN_DECL));
                self.drain_raw_up_to(extern_span.start);
            }
            None => {
                self.drain_raw_up_to(extern_span.start);
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::EXTERN_DECL));
            }
        }
        self.bump_into(SyntaxKind::EXTERN_TOK);

        // Return type (`cRetType = opt_attributes (cType | VOID)`). The optional
        // attributes and the C type are elided by the normaliser (the return type
        // reaches the compared AST only through the synthetic RHS's `Typed` wrapper).
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::EXTERN_RET));
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) {
            self.parse_attribute_lists();
        }
        self.parse_extern_ctype("extern", true);
        self.builder.finish_node(); // EXTERN_RET

        // `opt_access` (`extern int private c()`) — consumed and elided.
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Internal | Token::Private | Token::Public
                )),
                _,
            ))
        ) {
            self.bump_into(SyntaxKind::ACCESS_TOK);
        }

        // The prototype name — FCS's `ident`, a *single* identifier (emitted as a
        // one-segment `LONG_IDENT` to match FCS's `SynLongIdent([nm])`). It is
        // *not* a dotted path (`extern int A.B()`) or `global`
        // (`extern int global()`): FCS rejects both, so consume only an
        // `Ident`/`QuotedIdent` and leave a `.`/keyword to error, rather than
        // claiming a `LONG_IDENT` path and diverging into a valid-looking prototype.
        if let Some((_, span)) = self.peek() {
            let start = span.start;
            self.drain_raw_up_to(start);
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                _,
            ))
        ) {
            self.bump_into(SyntaxKind::IDENT_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected an identifier for the extern declaration name".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // LONG_IDENT

        // `( externArgs )`. The `(` is adjacent to the name, so LexFilter precedes
        // it with a zero-width `HighPrecedenceParenApp` marker.
        if self.peek_is_paren_app_marker() {
            self.bump_into(SyntaxKind::ERROR);
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LParen)), _))
        ) {
            self.bump_into(SyntaxKind::LPAREN_TOK);
            self.parse_extern_args();
            // LexFilter's high-precedence application paren *swallows* the closing
            // `)` (it never reaches the filtered stream), so recover it from the raw
            // stream — the object-expression `}` / primary-ctor `)` discipline.
            self.bump_swallowed_closer(
                SyntaxKind::RPAREN_TOK,
                |t| matches!(t, Token::RParen),
                ")",
                "extern argument list",
            );
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `(` after extern declaration name".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // EXTERN_DECL
    }

    /// Parse a `#`-directive (FCS's `hashDirective: HASH IDENT hashDirectiveArgs`,
    /// `pars.fsy:482`) into a [`SyntaxKind::HASH_DIRECTIVE_DECL`] —
    /// `SynModuleDecl.HashDirective(ParsedHashDirective(ident, args, _), _)`. The
    /// directive name is a single `IDENT_TOK`; each argument
    /// (`hashDirectiveArg: string | INT32 | IDENT`) is a string literal or `int32`
    /// (a `CONST_EXPR`, reusing [`Self::parse_const_expr`]) or a source identifier
    /// such as `__SOURCE_DIRECTORY__` (an `IDENT_TOK`). Caller has verified the
    /// cursor is at the raw `#` (`Token::Hash`).
    pub(super) fn parse_hash_directive(&mut self) {
        let hash_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_hash_directive invoked without a peeked `#`");
        self.drain_raw_up_to(hash_span.start);
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::HASH_DIRECTIVE_DECL));
        self.bump_into(SyntaxKind::HASH_TOK);
        // Directive name.
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                _,
            ))
        ) {
            self.bump_into(SyntaxKind::IDENT_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a directive name after `#`".to_string(),
                span,
            });
        }
        // Arguments (`hashDirectiveArg*`): string / int literals and source
        // identifiers, on the directive's own line. Stops at the line-ending layout
        // virtual (`ODECLEND` / `OBLOCKSEP`) or any non-argument token.
        loop {
            match self.peek() {
                Some((
                    Ok(FilteredToken::Raw(
                        Token::String
                        | Token::VerbatimString
                        | Token::TripleString
                        | Token::Int(_)
                        | Token::XInt(_),
                    )),
                    _,
                )) => self.parse_const_expr(),
                // A source identifier (`__SOURCE_DIRECTORY__` / `__SOURCE_FILE__` /
                // `__LINE__`, lexed as `Token::KeywordString`) or a plain ident —
                // FCS's `ParsedHashDirectiveArgument.SourceIdentifier`. Emitted as
                // `IDENT_TOK`; the normaliser reads its text (post-name `IDENT_TOK`s
                // are the source-identifier args).
                Some((
                    Ok(FilteredToken::Raw(
                        Token::Ident(_) | Token::QuotedIdent(_) | Token::KeywordString(_),
                    )),
                    _,
                )) => self.bump_into(SyntaxKind::IDENT_TOK),
                _ => break,
            }
        }
        self.builder.finish_node(); // HASH_DIRECTIVE_DECL
    }

    /// Parse the comma-separated `externArgs` (FCS's `externArgs`) into
    /// [`SyntaxKind::EXTERN_ARG`] children of the open `EXTERN_DECL`. Empty for
    /// `extern f()`.
    fn parse_extern_args(&mut self) {
        // The closing `)` is swallowed by LexFilter, so an *empty* list
        // (`extern f()`) has no filtered token between `(` and the swallowed `)` —
        // detect it on the raw stream (the next significant raw is the `)`).
        if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::RParen)) {
            return;
        }
        loop {
            self.parse_extern_arg();
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Comma)), _))) {
                self.bump_into(SyntaxKind::COMMA_TOK);
            } else {
                break;
            }
        }
    }

    /// Parse one `externArg` (FCS's `externArg = opt_attributes cType [ident]`)
    /// into an [`SyntaxKind::EXTERN_ARG`]: optional attributes, the C type, and an
    /// optional argument name (absent → an unnamed `SynPat.Wild`).
    fn parse_extern_arg(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::EXTERN_ARG));
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) {
            self.parse_attribute_lists();
        }
        self.parse_extern_ctype("extern argument", false);
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                _,
            ))
        ) {
            self.bump_into(SyntaxKind::IDENT_TOK);
        }
        self.builder.finish_node(); // EXTERN_ARG
    }

    /// Parse FCS's `cType`: a path or `void`, followed by zero or more C-style
    /// suffixes (`&`, `*`, or `[]`). The suffixes stay as direct tokens under the
    /// `EXTERN_RET` / `EXTERN_ARG`; the differential normaliser reconstructs the
    /// nested `SynType.App` wrappers FCS uses for these C-only spellings.
    fn parse_extern_ctype(&mut self, context: &str, allow_bare_void: bool) {
        let saw_void = matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Void)), _)));
        if saw_void {
            self.bump_into(SyntaxKind::VOID_TOK);
        } else {
            self.parse_long_ident_path(context);
        }

        let mut suffixes = Vec::new();
        loop {
            match self.next_non_trivia_raw_at_pos() {
                Some(Token::Amp)
                    if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Amp)), _))) =>
                {
                    let span = self
                        .peek()
                        .map(|(_, span)| span.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.bump_into(SyntaxKind::AMP_TOK);
                    suffixes.push((SyntaxKind::AMP_TOK, span));
                }
                Some(Token::Op("*"))
                    if matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Raw(Token::Op("*"))), _))
                    ) =>
                {
                    let span = self
                        .peek()
                        .map(|(_, span)| span.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.bump_into(SyntaxKind::STAR_TOK);
                    suffixes.push((SyntaxKind::STAR_TOK, span));
                }
                Some(Token::LBrack) => {
                    if matches!(
                        self.peek(),
                        Some((
                            Ok(FilteredToken::Virtual(Virtual::HighPrecedenceBrackApp)),
                            _
                        )),
                    ) {
                        self.bump_into(SyntaxKind::ERROR);
                    }
                    if !matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Raw(Token::LBrack)), _))
                    ) {
                        break;
                    }
                    let span = self
                        .peek()
                        .map(|(_, span)| span.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.bump_into(SyntaxKind::LBRACK_TOK);
                    suffixes.push((SyntaxKind::LBRACK_TOK, span));
                    match self.next_non_trivia_raw_at_pos() {
                        Some(Token::RBrack)
                            if matches!(
                                self.peek(),
                                Some((Ok(FilteredToken::Raw(Token::RBrack)), _))
                            ) =>
                        {
                            self.bump_into(SyntaxKind::RBRACK_TOK);
                        }
                        _ => {
                            let span = self
                                .next_non_trivia_raw_at_pos_with_span()
                                .map(|(_, span)| span)
                                .unwrap_or_else(|| self.source.len()..self.source.len());
                            self.errors.push(ParseError {
                                message: "expected `]` to close extern array type suffix"
                                    .to_string(),
                                span,
                            });
                        }
                    }
                }
                _ => break,
            }
        }

        if saw_void {
            let first_suffix = suffixes.first();
            let valid_void = match first_suffix {
                Some((SyntaxKind::STAR_TOK, _)) => true,
                None => allow_bare_void,
                Some(_) => false,
            };
            if !valid_void {
                let span = first_suffix
                    .map(|(_, span)| span.clone())
                    .or_else(|| self.peek().map(|(_, span)| span.clone()))
                    .unwrap_or(self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `*` after `void` extern C type".to_string(),
                    span,
                });
            }
        }
    }

    /// Emit a single `BINDING` node from the current cursor: consume the
    /// optional `inline` / `mutable` modifiers, parse the LHS pattern, then
    /// (if it succeeded) `=`, optional `OBLOCKBEGIN`, RHS expression, and
    /// any trailing in-block tokens. A failed LHS still opens and closes
    /// the `BINDING` node (zero-width when the failing token is left in
    /// place for the outer loop) so the `LET_DECL`'s child shape stays
    /// predictable across error paths.
    pub(super) fn parse_binding(&mut self) {
        self.parse_binding_with_modifiers(true);
    }

    /// As [`Self::parse_binding`], but `allow_modifiers` gates the
    /// `inline`/`mutable` prefix. Computation-expression bang binders
    /// (`let!`/`use!`/`and!`) pass `false`: FCS's `OBINDER headBindingPattern
    /// EQUALS …` production has no `opt_inline`/`opt_mutable`, so `let! mutable
    /// x = …` is "Invalid declaration syntax" with `isMutable = false` — *not* a
    /// mutable binding. Leaving the modifier unconsumed makes
    /// `parse_head_binding_pat` reject the `mutable`/`inline` keyword (recording
    /// the error) while keeping the binding's flags false, matching FCS.
    ///
    /// The `: T` return-type annotation is parsed for **both** forms (FCS's
    /// `AllowTypedLetUseAndBang`: a typed bang binder `let! x : T = …` sets the
    /// binding's `returnInfo`). The two differ only in how FCS records it: a
    /// regular binding's `mkSynBindingRhs` *also* wraps the RHS in
    /// `SynExpr.Typed(rhs, T)`, whereas a bang binder's `ceBindingCore` sets
    /// `returnInfo` without the `Typed` wrapper. We emit the same
    /// `BINDING_RETURN_INFO` node for both; the differential normaliser
    /// synthesises the `Typed` wrapper only for non-bang bindings (keyed on the
    /// binding's leading keyword), so the bang form stays unwrapped to match.
    pub(super) fn parse_binding_with_modifiers(&mut self, allow_modifiers: bool) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::BINDING));
        // A leading attribute run between the `let`/`and` keyword and the pattern
        // — FCS's `localBinding: attributes opt_access opt_inline opt_mutable
        // headBindingPattern`, `let [<Literal>] x = …`. The lists land as leading
        // `ATTRIBUTE_LIST` children of the `BINDING`, the same
        // `SynBinding.attributes` slot as the pre-`let` form (`[<A>] let x`, whose
        // attributes are consumed at decl level before this runs). Parsed before
        // the modifiers, as the FCS production orders them. Gated on
        // `allow_modifiers` (the plain-`let` contexts): a computation-expression
        // bang binder's `OBINDER headBindingPattern` production carries no
        // attributes, so `let! [<A>] x = e` is invalid F# — leave the `[<` for the
        // pattern parse to reject, matching FCS.
        if allow_modifiers
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
            )
        {
            self.parse_attribute_lists();
        }
        if allow_modifiers {
            self.parse_binding_modifiers();
        }
        if self.parse_head_binding_pat() {
            self.parse_binding_return_info();
            // `allow_modifiers` is precisely the `localBinding`-vs-CE-binder
            // discriminator: `true` for a `let`/`use` binding (FCS's
            // `localBinding`, which admits static optimization), `false` for a
            // computation-expression `let!`/`use!`/`and!` (`ceBindingCore`,
            // which does not). So it doubles as the static-optimization gate.
            self.parse_let_equals_rhs(allow_modifiers);
        }
        self.builder.finish_node();
    }

    /// Consume an optional return-type annotation between a binding head and
    /// its `=` — FCS's `opt_topReturnTypeWithTypeConstraints` (`pars.fsy:6039`,
    /// reached from `localBinding`/`memberCore`). Emits
    /// `BINDING_RETURN_INFO > [COLON_TOK, <type>]` when the cursor is at a
    /// `:`; a no-op otherwise.
    ///
    /// Safe to call after [`Self::parse_head_binding_pat`] for a regular
    /// binding: a bare binding head parses with `PatCtx::Head`, whose
    /// typed-pat colon arm is inert (a trailing `:` is `SynBinding.returnInfo`,
    /// not a [`SyntaxKind::TYPED_PAT`]), so the head pattern never swallows the
    /// colon. Function-form heads stop their curried-arg sweep at the `:`
    /// (not an atomic-pat start) for the same reason, so `let f x : T = …`
    /// attaches `T` here rather than to the last parameter. Also called for
    /// bang binders (`let! x : T = …`) — see [`Self::parse_binding_with_modifiers`].
    pub(super) fn parse_binding_return_info(&mut self) {
        if !matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
            return;
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::BINDING_RETURN_INFO));
        self.bump_into(SyntaxKind::COLON_TOK);
        // `topTypeWithTypeConstraints` (`opt_topReturnTypeWithTypeConstraints`,
        // `pars.fsy:6039`): the return type may carry a trailing `when` constraint
        // clause (`let f x : 'T when 'T : struct = …`) and, being a `topType`, a
        // named/optional parameter (`let f : x: int -> int = …`,
        // `member _.M : x: int -> int = …`) lowers to
        // `SynType.SignatureParameter`. Route through the `top` wrapper (phase
        // 10.12b); we elide FCS's `topType` arity regardless.
        self.parse_type_with_constraints_top();
        self.builder.finish_node();
    }

    /// `pars.fsy` `localBinding: ... opt_inline opt_mutable headBindingPattern`
    /// — consume the optional `inline` then the optional `mutable`. Both
    /// flow through LexFilter as raw tokens (`Token::Inline` /
    /// `Token::Mutable`). The order is significant: FCS rejects the
    /// reverse (`let mutable inline x = …`) with FS0010 at the trailing
    /// `inline`. We mirror the diagnostic but recover by consuming the
    /// trailing `inline` anyway, so both modifier tokens survive in the
    /// green tree and the `BINDING` shape stays predictable for the typed-AST
    /// projection.
    pub(super) fn parse_binding_modifiers(&mut self) {
        let saw_inline_first = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Inline)), _))
        );
        if saw_inline_first {
            self.bump_into(SyntaxKind::INLINE_TOK);
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Mutable)), _))
        ) {
            self.bump_into(SyntaxKind::MUTABLE_TOK);
        }
        if !saw_inline_first
            && let Some((Ok(FilteredToken::Raw(Token::Inline)), span)) = self.peek().cloned()
        {
            self.errors.push(ParseError {
                message: "unexpected `inline` after `mutable`; \
                          `inline` must precede `mutable` in a binding"
                    .to_string(),
                span,
            });
            self.bump_into(SyntaxKind::INLINE_TOK);
        }
    }

    /// Walk forward through `Virtual::BlockEnd` and `Virtual::BlockSep`
    /// filtered tokens from the cursor (the in-scope sentinels emitted at
    /// the end of a binding's RHS block) and test the next non-skippable
    /// filtered token with `pred`. Stops — returning `false` without
    /// consulting `pred` — at any other virtual (notably `Virtual::DeclEnd`,
    /// which signals that LexFilter has closed the surrounding
    /// `CtxtLetDecl`).
    ///
    /// Used by `parse_let_head_and_bindings` to detect an `and`-chain continuation
    /// without speculatively consuming the closing sentinels and without
    /// folding an offside `and` (one whose column is strictly less than
    /// `let`'s) into the prior declaration.
    pub(super) fn next_token_past_rhs_close_is(
        &self,
        pred: impl Fn(&FilteredToken<'_>) -> bool,
    ) -> bool {
        let mut i = self.pos;
        loop {
            match self.filtered_tokens.get(i) {
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd | Virtual::BlockSep)), _)) => {
                    i += 1
                }
                Some((Ok(FilteredToken::Virtual(_)), _)) => return false,
                Some((Ok(tok), _)) => return pred(tok),
                _ => return false,
            }
        }
    }
}
