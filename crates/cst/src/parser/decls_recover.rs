//! Error-recovery skip helpers for malformed type / signature bodies: consume
//! tokens as `ERROR` across offside blocks until a clean re-sync point.

use super::*;

impl<'src> Parser<'src> {
    /// Skip the body remainder of an unsupported signature type definition whose
    /// header has already been parsed — a blockless body (a column-0
    /// object-model member run, or a bodyless/`with` form). Consumes tokens as
    /// `ERROR` until a layout virtual or top separator ends the body, descending
    /// into any offside block (a `with`-augmentation's member block) via
    /// [`Self::skip_offside_block_as_error`].
    pub(super) fn skip_type_sig_body_remainder(&mut self) {
        while let Some((res, _)) = self.peek() {
            match res {
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => {
                    self.skip_offside_block_as_error();
                    return;
                }
                // A layout boundary (the body's dedent / inter-decl separator) or
                // a top separator ends the body — leave it for the caller's loop.
                Ok(FilteredToken::Virtual(_))
                | Ok(FilteredToken::Raw(Token::Semi | Token::SemiSemi)) => return,
                _ => self.bump_into(SyntaxKind::ERROR),
            }
        }
    }

    /// Record one [`TYPE_SIG_UNSUPPORTED_BODY_ERROR`] at the current token (or at
    /// end-of-source when the stream is exhausted).
    pub(super) fn push_type_sig_unsupported_body_error(&mut self) {
        let span = self
            .peek()
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| self.source.len()..self.source.len());
        self.errors.push(ParseError {
            message: TYPE_SIG_UNSUPPORTED_BODY_ERROR.to_string(),
            span,
        });
    }

    /// Record a diagnostic for an attribute run on a member-sig carrier this slice
    /// does not model — an `inherit` / `interface` member sig (slice 8 supports
    /// attributes only on `member`/`abstract`/`static member` / `new` / `val`-field
    /// sigs). The parsed `ATTRIBUTE_LIST`s stay as bare children; the item itself
    /// is still parsed by the caller.
    pub(super) fn push_attributed_member_sig_unsupported_error(&mut self) {
        let span = self
            .next_non_trivia_raw_at_pos_with_span()
            .map(|(_, s)| s)
            .unwrap_or_else(|| self.source.len()..self.source.len());
        self.errors.push(ParseError {
            message: "attributes on this member signature are a later phase-10 slice".to_string(),
            span,
        });
    }

    /// Skip the *interior* of an already-opened offside block — the cursor sits
    /// just after the opening `OBLOCKBEGIN` — consuming tokens as `ERROR` up to
    /// and including the matching `OBLOCKEND` (depth-tracked, mirroring
    /// [`Self::skip_offside_block_as_error`], which instead starts *at* the
    /// opener). Used to drop an unsupported signature type-definition body whose
    /// `OBLOCKBEGIN` was already claimed by [`Self::parse_sig_type_defn_repr`].
    pub(super) fn skip_offside_block_interior_as_error(&mut self) {
        let mut depth = 1u32;
        while let Some((res, _)) = self.peek() {
            match res {
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => {
                    depth += 1;
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                }
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) => {
                    depth -= 1;
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => self.bump_into(SyntaxKind::ERROR),
            }
        }
    }

    /// Skip a *stray* top-level `and` continuation — the real filtered `and`
    /// keyword plus the (type-definition-shaped) continuation's header + body, as
    /// `ERROR`. A *valid* `and`-chain is consumed by [`Self::parse_sig_type_defn`]'s
    /// chain loop (slice 5), so an `and` that reaches the module-decl loop is
    /// always malformed — a continuation after a non-type decl (`val x`⏎`and B =
    /// …`), which FCS rejects and drops. Skipping the whole continuation (header +
    /// body) as one error keeps a nested member spec (`and B =`⏎`  val q`)
    /// *contained*, rather than leaking it as a phantom top-level
    /// `SynModuleSigDecl.Val`. The caller's loop dispatches each `Token::And` here
    /// in turn, so a malformed run of continuations is fully skipped. Caller has
    /// verified the cursor is at a filtered `Token::And`.
    pub(super) fn skip_stray_type_continuation(&mut self) {
        let and_span = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::And)), span)) => span.clone(),
            _ => unreachable!("skip_stray_type_continuation invoked without an `and`"),
        };
        self.errors.push(ParseError {
            message: "unexpected `and`: a type-definition continuation here has no preceding \
                      type definition"
                .to_string(),
            span: and_span,
        });
        self.bump_into(SyntaxKind::ERROR); // the `and` keyword
        self.skip_one_type_header_and_body();
    }

    /// Skip one type definition's header + optional offside body as `ERROR`,
    /// stopping *before* any further `and`-continuation and any trailing layout
    /// virtual (left for the caller's loop). Used by
    /// [`Self::skip_stray_type_continuation`] to contain a malformed `and`
    /// continuation. The type-defn grammar is closed, so the shapes are
    /// exhaustive:
    /// * a `HighPrecedenceTyApp` virtual is a typar `<` (`type C<'T> = …`) — a
    ///   *header* virtual, consumed and stepped over (not a boundary);
    /// * a leading `[<…>]` is an after-keyword attribute (`and [<A>] …`) — the
    ///   list (and any offside `BlockSep` before the next-line name) is consumed
    ///   so the name/body still skip;
    /// * a `BlockBegin` opens the type's offside extent (FCS opens one after `=`
    ///   for an abbreviation *and* for the indented object-model body) — the whole
    ///   (depth-tracked) block is consumed via [`Self::skip_offside_block_as_error`]
    ///   so any nested member spec stays inside the skipped definition;
    /// * a header `;`/`;;` (before any block) is a top separator ending a
    ///   body-less / opaque type (`type T; val x`, valid) — stop there;
    /// * any other (layout) virtual ends a body-less `type C` (no `=`, no block).
    ///
    /// A *newline*-separated following spec is thus always preserved, and a
    /// `;`-separated *member* run inside the body stays contained (its `;` is
    /// inside the `=` block, never seen here).
    fn skip_one_type_header_and_body(&mut self) {
        while let Some((res, _)) = self.peek() {
            match res {
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => {
                    self.skip_offside_block_as_error();
                    return;
                }
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)) => {
                    // A typar `<` (`type C<'T>`) — part of the header, not a
                    // boundary. Step over it (zero-width, like the typar parser).
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                }
                Ok(FilteredToken::Raw(Token::LBrackLess)) => {
                    // An after-keyword attribute (`and [<A>] …`). Parse the list
                    // properly (a well-formed `ATTRIBUTE_LIST`, not ERROR), then
                    // step over any offside `BlockSep` before the (next-line) name
                    // so the header continuation isn't mistaken for a terminator.
                    self.parse_attribute_lists();
                    while matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                    ) {
                        self.builder
                            .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                        self.pos += 1;
                    }
                }
                // A same-line top separator (`;`/`;;`) in the *header* — before any
                // body block — ends a body-less / opaque type. Stop so the
                // following spec is re-dispatched by the caller.
                Ok(FilteredToken::Raw(Token::Semi | Token::SemiSemi)) => return,
                Ok(FilteredToken::Virtual(_)) => return,
                _ => self.bump_into(SyntaxKind::ERROR),
            }
        }
    }

    /// Consume a balanced offside block (`OBLOCKBEGIN` … matching `OBLOCKEND`) as
    /// `ERROR`, depth-tracking nested blocks. Caller has verified the cursor is at
    /// the opening `OBLOCKBEGIN`. Each block virtual is consumed zero-width (a
    /// manual `pos` bump, no raw drain) so a coinciding lexfilter-swallowed
    /// delimiter / keyword is not stolen — mirroring [`Self::bump_layout_virtual`]
    /// and the close handling in [`Self::drain_and_consume_offside_block_end`];
    /// interior non-virtual tokens are bumped as `ERROR` (which drains their
    /// leading trivia normally).
    fn skip_offside_block_as_error(&mut self) {
        let mut depth = 0u32;
        while let Some((res, _)) = self.peek() {
            match res {
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => {
                    depth += 1;
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                }
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) => {
                    depth -= 1;
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => self.bump_into(SyntaxKind::ERROR),
            }
        }
    }

    /// Consume the remainder of the *current* offside block as `ERROR`, stopping
    /// *before* the matching (depth-0) `OBLOCKEND` (left for the caller's
    /// close-drain). Unlike [`Self::skip_offside_block_as_error`], the caller is
    /// already *inside* the block (its opening `OBLOCKBEGIN` consumed), so this
    /// starts at depth 0 and depth-tracks any *nested* blocks. Used to contain a
    /// member-block tail a slice does not yet model — e.g. a signature `with`
    /// augmentation member kind `parse_sig_member_block_items` leaves at the
    /// cursor (an attributed `[<A>] member …`) — so it is not reprocessed as a
    /// sibling spec. Block virtuals are consumed zero-width; interior tokens bump
    /// as `ERROR` (draining trivia). A no-op when already at the closing
    /// `OBLOCKEND`.
    pub(super) fn skip_to_enclosing_block_end(&mut self) {
        let mut depth = 0u32;
        while let Some((res, _)) = self.peek() {
            match res {
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) if depth == 0 => return,
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => {
                    depth += 1;
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                }
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) => {
                    depth -= 1;
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                }
                _ => self.bump_into(SyntaxKind::ERROR),
            }
        }
    }

    /// Consume an object-expression-style `OWITH … OEND` block
    /// ([`Virtual::With`] … [`Virtual::End`]) as `ERROR`. The LexFilter emits this
    /// form — rather than the augmentation's `Raw(with) OBLOCKBEGIN … OBLOCKEND` —
    /// when a `with` augmentation's first member begins on the *same line* with a
    /// leading `[<…>]` attribute or access modifier (`exception E with [<A>] member
    /// …` / `with private member …`). Those augment-member forms are a later slice,
    /// so this contains the whole block inside the carrier node rather than letting
    /// it escape and be reprocessed as sibling specs. Caller has verified the
    /// cursor is at the `Virtual::With`. Stops *after* the matching depth-0 `OEND`
    /// (inclusive); a depth-0 `OBLOCKEND` or EOF reached first (malformed) stops
    /// without consuming it. Nested offside blocks are depth-tracked; block
    /// virtuals are consumed zero-width, interior tokens bump as `ERROR`.
    pub(super) fn skip_owith_block_as_error(&mut self) {
        // The `OWITH` itself (zero-width).
        self.builder
            .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
        self.pos += 1;
        let mut depth = 0u32;
        while let Some((res, _)) = self.peek() {
            match res {
                Ok(FilteredToken::Virtual(Virtual::End)) if depth == 0 => {
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                    return;
                }
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) if depth == 0 => return,
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => {
                    depth += 1;
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                }
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) => {
                    depth -= 1;
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                }
                _ => self.bump_into(SyntaxKind::ERROR),
            }
        }
    }
}
