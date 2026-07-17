//! Control-flow expression productions: `if`/`then`/`else`, `match`, `fun`
//! (lambda), and `function`.

use super::*;

impl<'src> Parser<'src> {
    /// `pars.fsy:4324 IF declExpr ifExprCases` — the
    /// `if c then e1 [(elif c2 then e2)* | else e2]` form. Phase 5.1
    /// added the basic three-part shape; Phase 5.2 promotes the no-else
    /// form to `SynExpr.IfThenElse(_, _, None, …)` (FCS encodes the
    /// missing else as `elseExpr = None`); Phase 5.3 lands `elif`
    /// chains and the same-line `else if` merge by nesting an inner
    /// IfThenElseExpr in the outer's else slot — matching FCS's
    /// `SynExpr.IfThenElse(_, _, Some(IfThenElse(_, _, _)), …)`
    /// encoding (the `isElif` distinction lives only in trivia and
    /// doesn't change the structural shape).
    ///
    /// Token mechanics (see `crates/cst/src/lexfilter/mod.rs:4070-4140`):
    /// the raw `Token::If` flows through as-is and is consumed normally.
    /// `Token::Then` and `Token::Else` are *rewritten* by LexFilter to
    /// [`Virtual::Then`] / [`Virtual::Else`] (with the keyword's source
    /// span preserved) followed by a [`Virtual::BlockBegin`] anchored at
    /// the body; we emit the keyword text under [`SyntaxKind::THEN_TOK`] /
    /// [`SyntaxKind::ELSE_TOK`] using the `LET_TOK`-style direct-emission
    /// path (the raw is still at `raw_pos` with the same span), and consume
    /// the `BlockBegin`/`BlockEnd` scaffolding as zero-width ERROR tokens.
    /// `Token::Elif` (and the same-line `else if` merge that LexFilter
    /// rewrites into a single `Token::Elif` with a span covering both
    /// keywords, see `crates/cst/src/lexfilter/mod.rs:3380-3408`) flows
    /// through as raw — emitted under [`SyntaxKind::ELIF_TOK`] with the
    /// merged source slice, then advance `raw_pos` past every raw fully
    /// contained in the merged span (Token::Else + WS + Token::If for
    /// the merged form, the single Token::Elif for the bare form).
    ///
    /// Caller must have already verified that `peek()` returns
    /// `FilteredToken::Raw(Token::If)`.
    pub(super) fn parse_if_then_else(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::IF_THEN_ELSE_EXPR));

        // `if` is a raw passthrough — `bump_into` drains trivia + emits the
        // keyword text at its source span.
        self.bump_into(SyntaxKind::IF_TOK);

        self.parse_if_then_else_tail();
    }

    /// Continuation of [`Self::parse_if_then_else`] after the leading
    /// `IF_TOK` (or, for an elif arm, the leading `ELIF_TOK`) has been
    /// emitted into an already-open `IF_THEN_ELSE_EXPR` node. Parses the
    /// condition, the `THEN_TOK` and then-body, then dispatches on the
    /// continuation: `Virtual::Else` for the else-body, `Token::Elif`
    /// for a nested chained arm (opens a fresh inner
    /// `IF_THEN_ELSE_EXPR` in the else slot and recurses), or nothing
    /// for the no-else form. Closes the open `IF_THEN_ELSE_EXPR` node
    /// before returning (both via the normal happy-path and the THEN-
    /// missing error path).
    pub(super) fn parse_if_then_else_tail(&mut self) {
        // Condition. `declExpr` per pars.fsy `IF declExpr ifExprCases`,
        // which includes tuples — `if a, b then …` parses with a tuple
        // condition. A `Virtual::Then` is not an infix or tuple
        // continuation, so `parse_expr` halts cleanly at the keyword
        // without us needing a custom stop predicate.
        if self.peek_is_expr_start() {
            self.parse_expr();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected expression after `if`".to_string(),
                span,
            });
        }

        // `Virtual::Then` — the rewrite of raw `Token::Then`. Same
        // mechanics as `Virtual::Let` (LexFilter shares the
        // raw-still-at-raw_pos invariant): drain trivia to the virtual's
        // start, emit the raw text as `THEN_TOK`, advance both cursors.
        // Going through `bump_into` would drain the raw as ERROR (no
        // `trivia_kind` matches) before reaching the virtual.
        if let Some((Ok(FilteredToken::Virtual(Virtual::Then)), then_span)) = self.peek().cloned() {
            self.drain_raw_up_to(then_span.start);
            debug_assert!(
                matches!(
                    self.raw_tokens.get(self.raw_pos),
                    Some((Ok(TriviaToken::Lexed(Token::Then)), s)) if *s == then_span,
                ),
                "Virtual::Then must be backed by a raw Token::Then at raw_pos with matching span"
            );
            self.emit_text(SyntaxKind::THEN_TOK, then_span);
            self.raw_pos += 1;
            self.pos += 1;
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `then` after `if` condition".to_string(),
                span,
            });
            self.builder.finish_node();
            return;
        }

        // `Virtual::BlockBegin` anchors the then-body's offside block
        // (LexFilter pushes a SeqBlock with `AddBlockEnd::Yes`). Consume
        // as a zero-width ERROR so the tree stays lossless without
        // inventing a dedicated kind. Track whether we opened one so
        // [`Self::parse_if_body`] can drain exactly the matching BlockEnd
        // — no more — preserving the count-balance that keeps a nested
        // no-else if (`if a then\n  if b then ()\nelse ()`) from
        // stealing the outer's else.
        let opened_then_block = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)),
        );
        if opened_then_block {
            self.bump_into(SyntaxKind::ERROR);
        }

        // Then-branch. `parse_if_body` handles single- and multi-
        // statement bodies (the latter wrapped in `SEQUENTIAL_EXPR`) and
        // releases exactly the BlockEnd we opened.
        self.parse_if_body("then", opened_then_block);

        // Continuation. Three cases:
        //
        // * `Virtual::Else`: same emission pattern as `Virtual::Then`,
        //   followed by a greedy `parse_if_body` for the else-body.
        //   Greedy via `parse_expr` inside `parse_if_body`: per
        //   `pars.fsy:323`, `expr_if` precedence sits below `COMMA`
        //   and below most infix levels, so `ELSE declExpr` greedily
        //   absorbs trailing infix and tuple commas into this branch
        //   (`if c then 1 else 2, 3` parses as
        //   `IfThenElse(c, 1, Tuple(2, 3))`).
        //
        // * `Token::Elif`: open a nested `IF_THEN_ELSE_EXPR` for the
        //   chained arm, emit the merged elif span as a single
        //   `ELIF_TOK`, and recurse into `parse_if_then_else_tail`. The
        //   recursive call closes the inner node; FCS's
        //   `IfThenElse(c1, e1, Some(IfThenElse(c2, e2, …)))`
        //   encoding falls out directly. Same shape covers both bare
        //   `elif` (one raw `Token::Elif`) and the same-line `else if`
        //   merge (two raws spanned by the filtered token).
        //
        // * Neither (no-else form): `parse_if_body` has already
        //   drained the matching BlockEnd, so the cursor sits on the
        //   outer scope's next token — falling through to `finish_node`
        //   here leaves no `ELSE_TOK`/`ELIF_TOK` or else child, which
        //   [`IfThenElseExpr::else_branch`] surfaces as `None`
        //   (FCS's `SynExpr.IfThenElse.elseExpr = None`).
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Virtual(Virtual::Else)), else_span)) => {
                self.drain_raw_up_to(else_span.start);
                debug_assert!(
                    matches!(
                        self.raw_tokens.get(self.raw_pos),
                        Some((Ok(TriviaToken::Lexed(Token::Else)), s)) if *s == else_span,
                    ),
                    "Virtual::Else must be backed by a raw Token::Else at raw_pos with matching span"
                );
                self.emit_text(SyntaxKind::ELSE_TOK, else_span);
                self.raw_pos += 1;
                self.pos += 1;

                let opened_else_block = matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)),
                );
                if opened_else_block {
                    self.bump_into(SyntaxKind::ERROR);
                }
                self.parse_if_body("else", opened_else_block);
            }
            Some((Ok(FilteredToken::Raw(Token::Elif)), elif_span)) => {
                // Bare `elif` vs merged `else if` (LexFilter rewrote both
                // into a single filtered `Token::Elif`, see
                // `crates/cst/src/lexfilter/mod.rs:3380-3408`). Dispatch
                // on the raw at `raw_pos`:
                //
                // * Bare `elif`: a single raw `Token::Elif` keyword. Emit
                //   a single `ELIF_TOK` as the first child of the nested
                //   `IF_THEN_ELSE_EXPR`. The presence of `ELIF_TOK` inside
                //   the nested node mirrors FCS's `isElif = true` trivia.
                //
                // * Merged `else if`: produce the same CST as the
                //   multi-line `else;\nif` form (which would have entered
                //   the `Virtual::Else` arm above): emit `ELSE_TOK` as a
                //   sibling of the nested node in the OUTER scope, drain
                //   trivia between the two keywords at the outer level
                //   (so e.g. `else (* c *) if` keeps the block comment
                //   addressable as its own `COMMENT` token rather than
                //   hiding it inside a keyword text run), then open the
                //   nested `IF_THEN_ELSE_EXPR` and emit `IF_TOK`. Matches
                //   FCS's `isElif = false` for the merged form — the
                //   structural distinction is `ELIF_TOK` vs `IF_TOK`
                //   leading the nested node.
                //
                // Drain trivia between the then-body and the elif keyword
                // into the OUTER scope (e.g. a comment on its own line
                // between arms). Mirrors the `Virtual::Else` arm.
                self.drain_raw_up_to(elif_span.start);

                let raw_kind = self
                    .raw_tokens
                    .get(self.raw_pos)
                    .and_then(|(res, _)| match res {
                        Ok(TriviaToken::Lexed(t)) => Some(t.clone()),
                        _ => None,
                    });
                match raw_kind {
                    Some(Token::Elif) => {
                        let elif_raw_span = self
                            .raw_tokens
                            .get(self.raw_pos)
                            .map(|(_, s)| s.clone())
                            .expect("raw_pos in range");
                        self.builder
                            .start_node(FSharpLang::kind_to_raw(SyntaxKind::IF_THEN_ELSE_EXPR));
                        self.emit_text(SyntaxKind::ELIF_TOK, elif_raw_span);
                        self.raw_pos += 1;
                    }
                    Some(Token::Else) => {
                        let else_raw_span = self
                            .raw_tokens
                            .get(self.raw_pos)
                            .map(|(_, s)| s.clone())
                            .expect("raw_pos in range");
                        self.emit_text(SyntaxKind::ELSE_TOK, else_raw_span);
                        self.raw_pos += 1;
                        // Locate the matching raw `Token::If` (LexFilter's
                        // merge only fires when both keywords are on the
                        // same line, so this must exist) and drain trivia
                        // between them — whitespace, block comments — at
                        // the OUTER scope so they're addressable as their
                        // own tokens.
                        let if_raw_span = self.raw_tokens[self.raw_pos..]
                            .iter()
                            .find_map(|(res, s)| {
                                matches!(res, Ok(TriviaToken::Lexed(Token::If))).then(|| s.clone())
                            })
                            .expect(
                                "merged Token::Elif must be backed by a following raw Token::If",
                            );
                        self.drain_raw_up_to(if_raw_span.start);
                        self.builder
                            .start_node(FSharpLang::kind_to_raw(SyntaxKind::IF_THEN_ELSE_EXPR));
                        self.emit_text(SyntaxKind::IF_TOK, if_raw_span);
                        self.raw_pos += 1;
                    }
                    other => {
                        unreachable!(
                            "Token::Elif filtered token must be backed by raw Token::Elif (bare) or Token::Else (merged) at raw_pos, got {other:?}",
                        );
                    }
                }
                self.pos += 1;

                // Depth-guarded: a long `elif` chain recurses here (each level
                // opens a nested `IF_THEN_ELSE_EXPR`) *below* `parse_pratt_expr`'s
                // guard, so count each level or the chain overflows the stack.
                //
                // Unlike the other call-site guards, the nested node opened just
                // above is closed by the *callee* (this function closes the node
                // open at its own entry — the `finish_node` below). When the guard
                // fires it skips that callee, so the nested node must be closed
                // here to keep the green tree balanced. (In practice the breach
                // surfaces in an `elif` *condition* first — itself a guarded,
                // balanced path — so this only hardens against the bare-recursion
                // breach, but an unbalanced builder is corruption either way.)
                if !self.with_depth_bool(|p| {
                    p.parse_if_then_else_tail();
                    true
                }) {
                    self.builder.finish_node(); // nested IF_THEN_ELSE_EXPR
                }
            }
            _ => {}
        }

        self.builder.finish_node(); // IF_THEN_ELSE_EXPR
    }

    /// `pars.fsy:4318 FUN atomicPatterns RARROW typedSeqExprBlockR` —
    /// the `fun <pat>+ -> <body>` lambda form. Phase 5.2 covers the
    /// single-arrow shape with one or more atomic argument patterns
    /// (`fun x -> x`, `fun x y -> x + y`, `fun (x, y) -> x`,
    /// `fun _ -> 0`, `fun () -> 1`); curried `Lambda`-chain projection
    /// is handled by the typed-AST facade. FCS's curried encoding is
    /// `Lambda(_, _, [p1], Lambda(_, _, [p2], body))` plus a
    /// `parsedData = Some(args, body)` cache on the outermost node;
    /// our green tree keeps the args flat under a single `FUN_EXPR`,
    /// and the projector reconstructs the chain when asked. Phase 5.4
    /// (or later) will add `match`-style `function | … -> …` sugar.
    ///
    /// Token mechanics (see `crates/cst/src/lexfilter/mod.rs:3218-3229`):
    /// `Token::Fun` is *rewritten* to [`Virtual::Fun`] with the same
    /// source span as the raw, so the raw still sits at `raw_pos` —
    /// emit the keyword text directly as [`SyntaxKind::FUN_TOK`] and
    /// advance both cursors (the `LET_TOK` / `THEN_TOK` pattern). The
    /// `->` pushes a one-sided SeqBlock (no opening `BlockBegin`,
    /// closes with [`Virtual::RightBlockEnd`]); the offside-pop of
    /// `CtxtFun` then emits [`Virtual::End`]. Both trail the body and
    /// are drained as zero-width ERROR tokens before this node closes.
    ///
    /// Caller must have already verified that `peek()` returns
    /// `FilteredToken::Virtual(Virtual::Fun)`.
    pub(super) fn parse_fun_expr(&mut self) {
        let fun_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_fun_expr invoked without a peeked Virtual::Fun");

        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::FUN_EXPR));

        // `Virtual::Fun` is LexFilter's rewrite of the raw `Token::Fun`
        // (LexFilter.fs:2532). Same emission pattern as `Virtual::Let`
        // / `Virtual::Then`: the raw still sits at `raw_pos` with the
        // same span as the virtual, so drain leading trivia (into the
        // node), assert the raw match, emit `FUN_TOK` directly, and
        // advance both cursors.
        self.drain_raw_up_to(fun_span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::Fun)), s)) if *s == fun_span,
            ),
            "Virtual::Fun must be backed by a raw Token::Fun at raw_pos with matching span"
        );
        self.emit_text(SyntaxKind::FUN_TOK, fun_span);
        self.raw_pos += 1;
        self.pos += 1;

        // Argument patterns. FCS's grammar is `FUN atomicPatterns
        // RARROW …` (pars.fsy:4318) where `atomicPatterns` is
        // `atomicPattern+`; each parameter is an *atomic* pat (ident,
        // wildcard, paren, unit), NOT an applPat — so `fun Some x ->
        // …` parses as two args `[Named "Some", Named "x"]`, not as a
        // single constructor pattern. We require at least one; missing
        // pattern is a parse error but we still try to consume the
        // arrow + body so recovery produces a well-formed FUN_EXPR.
        //
        // KNOWN DIVERGENCE — non-simple atomic patterns project
        // faithfully at the *pattern* level but diverge at the *body*
        // level. FCS runs `SimplePatsOfPat` (`pars.fsy:4310`) over the
        // parsed args: simple cases (Named, Wildcard, Paren wrapping
        // those, Unit, Tuple-paren of simples, Typed of simple) pass
        // through unchanged; non-simple cases (`ConstPat`, `NullPat`,
        // uppercase-`Ident` → `LongIdentPat`, paren-wrapped
        // constructor pats like `(Some x)`) are kept as the parsed
        // patterns but the lambda's body is rewritten to insert
        // generated `match` scaffolding for each non-simple arg. We
        // don't yet implement that lowering — so for inputs like
        // `fun X -> X`, `fun 0 -> 1`, `fun (Some x) -> x`, the green
        // tree is lossless but the typed-AST body shape diverges from
        // `SynExpr.Lambda.body`. Tracked separately; not in scope for
        // this slice. The five named shapes (Named lowercase,
        // Wildcard, Paren unit, Paren tuple, plain Named) all match
        // FCS exactly today.
        let mut arg_count: u32 = 0;
        while self.try_emit_atomic_pat() {
            arg_count += 1;
        }
        if arg_count == 0 {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected pattern after `fun`".to_string(),
                span,
            });
        }

        // `->` arrow. Raw passthrough (no virtual rewrite at this
        // position — LexFilter only pushes the one-sided SeqBlock and
        // does not replace the token, see LexFilter.fs:3302-3320).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::RArrow)), _))
        ) {
            self.bump_into(SyntaxKind::RARROW_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `->` after `fun` parameter patterns".to_string(),
                span,
            });
            self.builder.finish_node();
            return;
        }

        // Body. FCS's `typedSequentialExprBlockR` is a sequence of
        // expressions at the one-sided SeqBlock the `->` opened; each
        // statement is a full expression (including tuples — `pars.fsy:323`
        // puts FUN below `COMMA`, so `fun x -> 1, 2` parses as
        // `Lambda(x, Tuple(1, 2))`). Multi-statement bodies (`fun x ->\n
        // a\n    b`) are wrapped in `SEQUENTIAL_EXPR`, mirroring FCS's
        // `SynExpr.Sequential(SuppressNeither, a, b)`.
        //
        // Shared offside-block gather (see [`Self::parse_seq_block_body`]):
        // the first statement plus each same-indent `Virtual::BlockSep`
        // continuation, wrapped in `SEQUENTIAL_EXPR` when more than one.
        self.parse_seq_block_body("expected expression after `->`");

        // Drain the trailing scaffolding for *this* lambda only: the
        // `->` pushed a one-sided SeqBlock, which closes with
        // `Virtual::RightBlockEnd` when the offside cascade fires; the
        // surrounding `CtxtFun` then pops and emits `Virtual::End`.
        // Both arrive as zero-width virtuals (LexFilter's `insert_token`
        // convention) at the same source position.
        //
        // We must consume *exactly* this lambda's pair — no more. When
        // a lambda body is itself a lambda (`fun x -> fun y -> y\nz`),
        // LexFilter emits the inner and outer close pairs consecutively
        // at the same offset; a `while` loop that swallows every
        // `RightBlockEnd|End` would also consume the enclosing lambda's
        // close, and the outer `parse_fun_expr` would then absorb
        // sibling top-level decls into its body via the `BlockSep`
        // loop above (regression pinned by
        // `diff_ast_fun_lambda_nested_lambda_body_with_sibling`).
        //
        // We also must NOT route these through `bump_into` — that
        // helper drains raw tokens up to the *next filtered token's*
        // start as trivia (see [`Self::bump_into`]), which would pull
        // e.g. the closing `)` of a surrounding paren-expr or trailing
        // newline trivia into this `FUN_EXPR` node. The if-then-else
        // BlockEnd close uses the same "zero-width without drain"
        // pattern (see [`Self::parse_if_body`]) for the same reason.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::RightBlockEnd)), _)),
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::End)), _)),
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }

        self.builder.finish_node(); // FUN_EXPR
    }

    /// `match <scrut> with [|]? <pat> -> <result> [| <pat> -> <result>]*` —
    /// FCS's `SynExpr.Match` (`pars.fsy:4221 MATCH typedSequentialExpr
    /// withClauses`). Phase 5.M.2: one *or more* clauses with an optional
    /// leading `|` and `|` separators; still no `when` guard.
    ///
    /// Token mechanics (verified against the LexFilter output for
    /// `match x with A -> 1 | B -> 2`):
    /// ```text
    ///   Match  Ident(x)  With(V)  Ident(A)  RArrow  Int(1)  Bar  Ident(B)  RArrow  Int(2)  RightBlockEnd(V)  End(V)
    /// ```
    /// `Token::Match` is a plain raw token (LexFilter pushes `CtxtMatch`
    /// but does not rewrite the token), so it bumps directly. `with` is a
    /// [`Virtual::With`] *backed by* a raw `Token::With` at the same span
    /// (the `OWITH` relabel, the `FUN_TOK`/`LET_TOK` pattern): emit the
    /// keyword text and advance both cursors, or the raw `with` would be
    /// drained as an ERROR token.
    ///
    /// The `|` clause separator is a bare raw `Token::Bar`. There are two
    /// LexFilter shapes for the SeqBlock closes: on a *single line* only one
    /// trailing [`Virtual::RightBlockEnd`] is emitted (after the last
    /// clause), whereas *offside* (clauses on their own lines) each clause is
    /// closed by its own `RightBlockEnd` before the next `Bar`. Either way the
    /// enclosing `CtxtMatchClauses` pops exactly once, emitting one
    /// [`Virtual::End`]. All of these trail as zero-width virtuals.
    ///
    /// We drain *exactly* this match's `RightBlockEnd` + `End` — no more —
    /// so an enclosing `let`'s `BlockEnd`/`DeclEnd` survive for
    /// `parse_let_binding` (the `let f x = match …` case). Mirrors
    /// `parse_fun_expr`'s careful single-pair drain, and for the same
    /// reason the virtuals are stamped zero-width without `bump_into`
    /// (which would pull a surrounding paren's `)` or trailing newline
    /// trivia into this node).
    ///
    /// Caller must have verified that `peek()` returns
    /// `FilteredToken::Raw(Token::Match)`.
    pub(super) fn parse_match_expr(&mut self) {
        // `match` keyword — plain raw passthrough.
        self.parse_match_or_match_bang(SyntaxKind::MATCH_EXPR, SyntaxKind::MATCH_TOK, "match");
    }

    /// `match! <scrut> with <clauses>` — `SynExpr.MatchBang`
    /// (`SyntaxTree.fsi:916`), the computation-expression match binder.
    /// Field-for-field identical to `match` apart from the keyword and case
    /// name: `Token::MatchBang` is a plain raw token (LexFilter pushes
    /// `CtxtMatch`/`CtxtMatchClauses` but does not relabel it), so the filtered
    /// stream is `MatchBang expr With <clauses> RightBlockEnd End` — a clone of
    /// `match`'s. Shares the whole body (scrutinee + `with` + clause list) with
    /// [`Self::parse_match_expr`] via [`Self::parse_match_or_match_bang`].
    /// Caller must have verified `peek()` is `Raw(Token::MatchBang)`.
    pub(super) fn parse_match_bang_expr(&mut self) {
        self.parse_match_or_match_bang(
            SyntaxKind::MATCH_BANG_EXPR,
            SyntaxKind::MATCH_BANG_TOK,
            "match!",
        );
    }

    /// Shared core of `match`/`match!` (`SynExpr.Match` / `SynExpr.MatchBang`).
    /// The two forms differ only in the enclosing node (`node_kind`), the
    /// leading keyword token (`kw_kind`, both plain raw passthroughs), and the
    /// keyword text used in diagnostics (`keyword`); the scrutinee, the `with`
    /// relabel, and the clause list (FCS's `patternClauses`, shared with
    /// `function …` via [`Self::parse_match_clauses`]) are identical.
    fn parse_match_or_match_bang(
        &mut self,
        node_kind: SyntaxKind,
        kw_kind: SyntaxKind,
        keyword: &str,
    ) {
        self.builder.start_node(FSharpLang::kind_to_raw(node_kind));

        // `match`/`match!` keyword — plain raw passthrough.
        self.bump_into(kw_kind);

        // Scrutinee — FCS's `typedSequentialExpr` (`pars.fsy`): a statement
        // sequence that may carry a trailing `: T` annotation, so `match e : t
        // with` (`SynExpr.Typed`) and `match e1; e2 with` (`SynExpr.Sequential`)
        // are both scrutinees, not just a single expression. `parse_seq_block_body`
        // (`allow_typed = true`) parses exactly that and stops at the
        // `Virtual::With` (no separator consumes it); it reports the missing-first
        // error itself.
        self.parse_seq_block_body(&format!("expected expression after `{keyword}`"));

        // `with` keyword. `Virtual::With` is LexFilter's `OWITH` relabel of
        // the raw `Token::With` at the same span — emit the keyword and
        // advance both cursors (the `FUN_TOK` pattern).
        let Some((Ok(FilteredToken::Virtual(Virtual::With)), with_span)) = self.peek().cloned()
        else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: format!("expected `with` in `{keyword}` expression"),
                span,
            });
            self.builder.finish_node(); // node_kind
            return;
        };
        self.drain_raw_up_to(with_span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::With)), s)) if *s == with_span,
            ),
            "Virtual::With must be backed by a raw Token::With at raw_pos with matching span"
        );
        self.emit_text(SyntaxKind::WITH_TOK, with_span);
        self.raw_pos += 1;
        self.pos += 1;

        // Clause list (FCS's `patternClauses`), shared with `function …`.
        self.parse_match_clauses();

        self.builder.finish_node(); // node_kind
    }

    /// Parse the `|`-separated clause list shared by `match … with …` and
    /// `function …` (FCS's `patternClauses`, `pars.fsy:4958`). On entry the
    /// cursor sits at the first clause's optional leading `|` (or its
    /// pattern); on exit it has drained the trailing `CtxtMatchClauses`
    /// close (`Virtual::End`).
    ///
    /// The caller owns the enclosing node ([`SyntaxKind::MATCH_EXPR`] or
    /// [`SyntaxKind::MATCH_LAMBDA_EXPR`]) and must `finish_node()` it after
    /// this returns — including the early-bail path: a missing clause `->`
    /// finishes only the current `MATCH_CLAUSE` and returns, leaving the
    /// outer node for the caller to close.
    ///
    /// The clause pattern is FCS's `parenPattern`; we reuse the
    /// head-binding entry, which covers atomic / ctor-app (`Some y`) /
    /// tuple (`x, y`) / `as`. `in_paren = false` leaves the trailing `:`
    /// arm inert — a top-level typed clause pattern (`y : int ->`),
    /// or-patterns, isinst, and-patterns are deferred to later slices (they
    /// track phase-6 SynPat work).
    ///
    /// Two distinct LexFilter shapes are unified by this loop:
    ///
    /// * single-line (`match x with A -> 1 | B -> 2`): the `|` is a bare
    ///   raw `Bar` with no surrounding block-end, and exactly one trailing
    ///   `RightBlockEnd`+`End` closes the whole construct after the last
    ///   clause;
    /// * offside multi-line (clauses on separate lines): each clause is
    ///   closed by its own `RightBlockEnd` *before* the next `Bar`, then a
    ///   single final `End`.
    ///
    /// The per-clause optional-`RightBlockEnd` drain plus a `peek() == Bar`
    /// continuation check handles both. Each clause owns its leading
    /// `BAR_TOK` (clause 1's is the optional leading bar; later clauses'
    /// is the separator), mirroring FCS's per-clause `BarRange`.
    pub(super) fn parse_match_clauses(&mut self) {
        loop {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::MATCH_CLAUSE));

            // Optional leading `|` (clause 1) or separator `|` (later clauses).
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Bar)), _))) {
                self.bump_into(SyntaxKind::BAR_TOK);
            }

            // Clause pattern (FCS's `parenPattern`, head-binding entry). A
            // clause head — and every operand in its `,`/`&`/`|` tail — is a
            // full `parenPattern`, so a leading `[< … >]` is a valid attributed
            // pattern (`SynPat.Attrib`, phase 10.6) here just as inside parens
            // — verified against FCS (`match v with [<A>] x -> x`,
            // `match v with A | [<B>] x -> x`). `PatCtx::Clause` admits the
            // attribute prefix at both the head and the tail operands while
            // leaving the per-element `:` to the (erroring) FCS clause-`:` path.
            let pat_cp = self.builder.checkpoint();
            let pat_ok = if self.at_attribute_list_start() {
                self.emit_attrib_pat(pat_cp, PatCtx::Clause);
                true
            } else {
                self.try_emit_head_binding_pat_element()
            };
            if pat_ok {
                self.wrap_pat_tail(pat_cp, PatCtx::Clause);
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected pattern in `match` clause".to_string(),
                    span,
                });
            }

            // Optional `when` guard (FCS's `patternAndGuard`). `Token::When`
            // is a bare raw token (LexFilter pushes `CtxtWhen` but does not
            // relabel it); the guard is a normal expression that `parse_expr`
            // reads up to the clause `->` (`->` is consumed only by the type
            // parser and the fun/match parsers, never the expr climber).
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::When)), _))) {
                self.bump_into(SyntaxKind::WHEN_TOK);
                if self.peek_is_expr_start() {
                    self.parse_expr();
                } else {
                    let span = self
                        .peek()
                        .map(|(_, s)| s.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message: "expected guard expression after `when`".to_string(),
                        span,
                    });
                }
            }

            // `->` arrow. Raw passthrough (no virtual rewrite at this position).
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::RArrow)), _))
            ) {
                self.bump_into(SyntaxKind::RARROW_TOK);
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `->` after `match` clause pattern".to_string(),
                    span,
                });
                self.builder.finish_node(); // MATCH_CLAUSE
                return;
            }

            // Clause result — FCS's `typedSequentialExprBlockR`, a sequence
            // of expressions at the one-sided SeqBlock the `->` opened. A
            // single expression is the clause result directly; a multi-statement
            // body (statements separated by an offside `Virtual::BlockSep` or
            // an explicit `;` — `A -> e1; e2`) is wrapped in `SEQUENTIAL_EXPR`,
            // mirroring `parse_fun_expr`'s body handling and FCS's
            // `SynExpr.Sequential(SuppressNeither, …)`. The gather stops at this
            // clause's `RightBlockEnd`, so it never swallows the clause-list
            // close (`Virtual::End`) or a following clause's `Bar` separator —
            // see `diff_ast_match_seq_body_then_sibling_decl` /
            // `diff_ast_match_seq_body_in_multi_clause`.
            //
            // The gather runs *after* `RARROW_TOK` and after any `when` guard,
            // so its internal checkpoint (see [`Self::parse_seq_block_body`])
            // wraps only the result statements; the guard stays a separate
            // sibling `Expr` child and the positional `guard()`/`result()`
            // disambiguation is preserved.
            self.parse_seq_block_body("expected expression after `->`");

            // Drain this clause's one-sided SeqBlock close (zero-width), if
            // present. Single-line shapes emit it only after the last clause;
            // offside shapes emit one per clause.
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::RightBlockEnd)), _)),
            ) {
                self.builder
                    .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                self.pos += 1;
            }
            self.builder.finish_node(); // MATCH_CLAUSE

            // Continue while the next clause separator `|` is present. `peek`
            // skips trivia, so any `RightBlockEnd` drained above doesn't hide
            // the following `Bar`.
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Bar)), _))) {
                continue;
            }
            break;
        }

        // Drain the `CtxtMatchClauses` close (zero-width). Exactly one —
        // see the doc comment.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::End)), _)),
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }
    }

    /// `try body with <clauses>` — `SynExpr.TryWith` (`SyntaxTree.fsi:759`,
    /// FCS's `pars.fsy:4245 TRY typedSequentialExprBlockR withClauses`) — and
    /// `try body finally e` — `SynExpr.TryFinally` (`SyntaxTree.fsi:768`, FCS's
    /// `pars.fsy:4313 TRY typedSequentialExprBlockR FINALLY
    /// typedSequentialExprBlock`). Both share the try head + body; the keyword
    /// after the body (`with` vs `finally`) selects the form. `Token::Try` is a
    /// plain raw token (LexFilter pushes `CtxtTry` and opens a one-sided
    /// SeqBlock for the body, but does not relabel it).
    ///
    /// The **try head + body** is shared by both forms: the body is FCS's
    /// `typedSequentialExprBlockR` — a one-sided SeqBlock (no opening
    /// `BlockBegin`; closes with [`Virtual::RightBlockEnd`]), exactly a
    /// match-clause result body — so it goes through [`Self::parse_seq_block_body`]
    /// then a trailing `RightBlockEnd` drain (a multi-statement body wraps in
    /// [`SyntaxKind::SEQUENTIAL_EXPR`]).
    ///
    /// **TryWith** (filtered stream `Try <body> RightBlockEnd With <clauses>
    /// RightBlockEnd End`):
    ///
    /// * the **`with`** is LexFilter's `OWITH` ([`Virtual::With`]) relabel of a
    ///   raw `Token::With` at the same span — emitted with the backed-by-raw
    ///   `FUN_TOK`/`WITH_TOK` pattern shared with [`Self::parse_match_or_match_bang`];
    /// * the **clause list** (FCS's `withClauses` → `withPatternClauses`, the
    ///   same `patternClauses` non-terminal as `match`) reuses
    ///   [`Self::parse_match_clauses`] verbatim, including its trailing
    ///   `CtxtMatchClauses` `Virtual::End` drain.
    ///
    /// **TryFinally** (filtered stream `Try <body> RightBlockEnd Finally
    /// BlockBegin <fin-body> DeclEnd`):
    ///
    /// * `finally` is a plain raw `Token::Finally` passthrough (no relabel) →
    ///   [`SyntaxKind::FINALLY_TOK`];
    /// * the **finally body** is FCS's `typedSequentialExprBlock` — a *regular*
    ///   block (`BlockBegin … DeclEnd`), byte-identical to a `while`/`for`
    ///   `do`-body, so it reuses [`Self::parse_block_body_after_keyword`] (the
    ///   `BlockBegin` drain + [`Self::parse_if_body`] + [`Self::consume_block_decl_end`]
    ///   tail shared with [`Self::parse_do_block_body`]).
    ///
    /// The two forms are discriminated downstream by the presence of a
    /// `WITH_TOK` plus [`SyntaxKind::MATCH_CLAUSE`] children (TryWith) versus a
    /// `FINALLY_TOK` plus a trailing finally-body `Expr` (TryFinally). A missing
    /// `with`/`finally` bails cleanly — no panic, the partial `TRY_EXPR` closes
    /// and the leftover token is handled by the enclosing context.
    ///
    /// Precondition: the next non-trivia filtered token is `Raw(Token::Try)`.
    pub(super) fn parse_try_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TRY_EXPR));

        // `try` keyword — plain raw passthrough (like `match`/`while`/`for`).
        self.bump_into(SyntaxKind::TRY_TOK);

        // Body — FCS's `typedSequentialExprBlockR`. The one-sided SeqBlock the
        // `Token::Try` push opened has no leading `BlockBegin`; it closes with
        // a `Virtual::RightBlockEnd` after the body, which `parse_seq_block_body`
        // stops at (it never consumes its own close) and we drain below.
        self.parse_seq_block_body("expected expression after `try`");
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::RightBlockEnd)), _)),
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }

        // Branch on the keyword after the body: `with` (TryWith) or `finally`
        // (TryFinally). `finally` is checked first so its raw token isn't
        // mistaken for a missing-`with` bail.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Finally)), _)),
        ) {
            // `finally` — plain raw passthrough (no relabel), then the regular
            // block finally body (the `while`/`for` `do`-body shape).
            self.bump_into(SyntaxKind::FINALLY_TOK);
            self.parse_block_body_after_keyword("finally");
            self.builder.finish_node(); // TRY_EXPR
            return;
        }

        // `with` keyword. `Virtual::With` is LexFilter's `OWITH` relabel of the
        // raw `Token::With` at the same span — emit the keyword and advance both
        // cursors (the `FUN_TOK`/`WITH_TOK` pattern). A missing handler bails
        // cleanly without a panic.
        let Some((Ok(FilteredToken::Virtual(Virtual::With)), with_span)) = self.peek().cloned()
        else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `with` or `finally` in `try` expression".to_string(),
                span,
            });
            self.builder.finish_node(); // TRY_EXPR
            return;
        };
        self.drain_raw_up_to(with_span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::With)), s)) if *s == with_span,
            ),
            "Virtual::With must be backed by a raw Token::With at raw_pos with matching span"
        );
        self.emit_text(SyntaxKind::WITH_TOK, with_span);
        self.raw_pos += 1;
        self.pos += 1;

        // Clause list (FCS's `withClauses`), shared verbatim with `match … with`.
        self.parse_match_clauses();

        self.builder.finish_node(); // TRY_EXPR
    }

    /// `function pat -> e | …` — FCS's `SynExpr.MatchLambda`
    /// (`pars.fsy`). The `function` sugar for an anonymous
    /// single-argument `match`; FCS keeps it as a *distinct* parsed node
    /// (the `fun _argN -> match _argN with …` synthesis is a later,
    /// typecheck-time step, not part of `ParsedInput`), so we mirror it
    /// with [`SyntaxKind::MATCH_LAMBDA_EXPR`] rather than desugaring.
    ///
    /// There is no scrutinee and no `with`: LexFilter rewrites the raw
    /// `Token::Function` to [`Virtual::Function`] (`OFUNCTION`) at the
    /// same span and pushes `CtxtFunction` + `CtxtMatchClauses`, so the
    /// clause list (and its trailing `RightBlockEnd`/`End` scaffolding)
    /// is identical to `match`'s — we reuse [`Parser::parse_match_clauses`]
    /// verbatim. The keyword is emitted with the same backed-by-raw
    /// pattern as [`Parser::parse_fun_expr`]'s `FUN_TOK` / `parse_match_expr`'s
    /// `WITH_TOK`.
    ///
    /// Precondition: the next non-trivia filtered token is
    /// `FilteredToken::Virtual(Virtual::Function)`.
    pub(super) fn parse_function_expr(&mut self) {
        let fn_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_function_expr invoked without a peeked Virtual::Function");

        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::MATCH_LAMBDA_EXPR));

        // `Virtual::Function` is LexFilter's `OFUNCTION` relabel of the raw
        // `Token::Function` at the same span (the `FUN_TOK` / `WITH_TOK`
        // pattern): drain leading trivia into the node, assert the raw
        // match, emit `FUNCTION_TOK` directly, and advance both cursors.
        self.drain_raw_up_to(fn_span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::Function)), s)) if *s == fn_span,
            ),
            "Virtual::Function must be backed by a raw Token::Function at raw_pos with matching span"
        );
        self.emit_text(SyntaxKind::FUNCTION_TOK, fn_span);
        self.raw_pos += 1;
        self.pos += 1;

        // Clause list (FCS's `patternClauses`), shared with `match`.
        self.parse_match_clauses();

        self.builder.finish_node(); // MATCH_LAMBDA_EXPR
    }

    /// `while cond do body` — `SynExpr.While` (`SyntaxTree.fsi:656`,
    /// `pars.fsy:4367`). `Token::While` is a plain raw token (LexFilter pushes
    /// `CtxtWhile` but does not relabel it); the `do` keyword surfaces as
    /// `Virtual::Do` backed by the raw `Token::Do` at the same span, and the
    /// body is a SeqBlock (`BlockBegin … BlockEnd DeclEnd`) parsed exactly like
    /// a `do!`/`if` body. Caller must have verified `peek()` is
    /// `Raw(Token::While)`.
    pub(super) fn parse_while_expr(&mut self) {
        self.parse_while_loop(SyntaxKind::WHILE_EXPR, SyntaxKind::WHILE_TOK, "while");
    }

    /// `while! cond do body` — `SynExpr.WhileBang` (`SyntaxTree.fsi:928`), the
    /// computation-expression while binder. Identical to plain `while` apart
    /// from the keyword and case name: `Token::WhileBang` is a plain raw token,
    /// so the filtered stream is a clone of `while`'s and the body parse is
    /// shared via [`Self::parse_while_loop`]. Caller must have verified
    /// `peek()` is `Raw(Token::WhileBang)`.
    pub(super) fn parse_while_bang_expr(&mut self) {
        self.parse_while_loop(
            SyntaxKind::WHILE_BANG_EXPR,
            SyntaxKind::WHILE_BANG_TOK,
            "while!",
        );
    }

    /// Shared core of `while`/`while!` (`SynExpr.While` / `SynExpr.WhileBang`,
    /// identical-shaped `whileExpr`/`doExpr` pairs). The two forms differ only
    /// in the enclosing node (`node_kind`), the leading keyword token
    /// (`kw_kind`, both plain raw passthroughs), and the keyword text used in
    /// diagnostics (`keyword`). `while!` (10.4e) wires a second caller and
    /// dispatch arm; the body-block parse is shared with `do!` via
    /// [`Parser::parse_if_body`].
    fn parse_while_loop(&mut self, node_kind: SyntaxKind, kw_kind: SyntaxKind, keyword: &str) {
        self.builder.start_node(FSharpLang::kind_to_raw(node_kind));

        // `while`/`while!` keyword — plain raw passthrough.
        self.bump_into(kw_kind);

        // Condition. `parse_expr` stops at the `Virtual::Do` (no expr
        // production consumes it) — the `match`-scrutinee/`Virtual::With`
        // pattern.
        if self.peek_is_expr_start() {
            self.parse_expr();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: format!("expected condition after `{keyword}`"),
                span,
            });
        }

        self.parse_do_block_body(keyword);

        self.builder.finish_node(); // node_kind
    }

    /// `for` loops — dispatch between the enumerator form (`for pat in e do …`,
    /// `SynExpr.ForEach`) and the range form (`for i = a to/downto b do …`,
    /// `SynExpr.For`). Caller must have verified `peek()` is `Raw(Token::For)`.
    ///
    /// FCS's `forLoopRange` is `parenPattern EQUALS …` and `forLoopBinder` is
    /// `parenPattern IN …` (`pars.fsy:5615`/`:5629`), disambiguated on the token
    /// following the pattern. For valid input the range binder reduces (via
    /// `idOfPat`, `ParseHelpers.fs:921`) to a *simple* ident — a plain/backtick
    /// ident (`SynPat.Named`/single-segment `LongIdent`) or the wildcard `_`
    /// (`SynPat.Wild`, the F# 4.7+ `WildCardInForLoop` feature) — so a leading
    /// `<ident|_> =` (checked on the raw lookahead past `for`; both tokens are
    /// raw, no intervening virtuals here) selects the range form. Everything
    /// else (a paren / tuple / typed pattern, or a binder head not followed by
    /// `=`) is the enumerator form. The `for … -> e` comprehension form is a
    /// separate slice.
    pub(super) fn parse_for_expr(&mut self) {
        // Raw lookahead past the `for` at the cursor: index 0 is `for`, 1 the
        // binder head, 2 the `=`/`in`/… that disambiguates the two forms.
        let is_range = matches!(
            self.nth_significant_raw_at_pos(1),
            Some(Token::Ident(_) | Token::QuotedIdent(_) | Token::Underscore)
        ) && matches!(self.nth_significant_raw_at_pos(2), Some(Token::Equals));
        if is_range {
            self.parse_for_range_loop();
        } else {
            self.parse_for_each_loop();
        }
    }

    /// The enumerator `for` loop — `SynExpr.ForEach` (`SyntaxTree.fsi:671`).
    /// `Token::For` is a plain raw token (LexFilter pushes `CtxtFor` but does not
    /// relabel it); the binder pattern is a `parenPattern` (the full operand
    /// form) and the `in` a raw `Token::In` left in the filtered stream
    /// (LexFilter's `in` arm is gated on `Context::LetDecl`, not `Context::For`).
    /// After the enumerable collection comes one of two bodies:
    ///
    /// * **`do body`** (`pars.fsy:4372`) — the statement form, sharing `while`'s
    ///   SeqBlock scaffolding via [`Self::parse_do_block_body`].
    /// * **`-> body`** (`pars.fsy:4412` / `arrowThenExprR`) — the comprehension
    ///   form (`seq { for x in xs -> x }`), handled by
    ///   [`Self::parse_for_arrow_body`].
    fn parse_for_each_loop(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::FOR_EACH_EXPR));

        // `for` keyword — plain raw passthrough.
        self.bump_into(SyntaxKind::FOR_TOK);

        // Binder pattern. FCS's `forLoopBinder` is `parenPattern IN declExpr`
        // (`pars.fsy:5615`), so this is a *full* `parenPattern` — `PatCtx::Paren`,
        // not the match-clause-head `Clause`. That admits the per-element `:`
        // typed-pat (`for x : int in xs` is valid F#, projecting to a `Typed`
        // binder), a leading `[< … >]` attribute, and the `,`/`&`/`as`/`::` tail;
        // the pattern parsers stop at the raw `Token::In` (not a pattern
        // continuation). `emit_pat_atom` is the unified element entry (attribute
        // hook + raw-start gate + the `Paren`/`Clause` element split).
        let pat_cp = self.builder.checkpoint();
        if self.emit_pat_atom(PatCtx::Paren) {
            self.wrap_pat_tail(pat_cp, PatCtx::Paren);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected pattern after `for`".to_string(),
                span,
            });
        }

        // `in` keyword — a raw `Token::In` left in the filtered stream (see
        // above), bumped directly like [`SyntaxKind::WHILE_TOK`].
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::In)), _))) {
            self.bump_into(SyntaxKind::IN_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `in` in `for` loop".to_string(),
                span,
            });
        }

        // Enumerable collection. `parse_expr` stops at the `Virtual::Do` (no
        // expr production consumes it) — the `while`-condition pattern.
        if self.peek_is_expr_start() {
            self.parse_expr();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected enumerable expression after `in`".to_string(),
                span,
            });
        }

        // The body is either `-> e` (the comprehension form) or `do <block>`
        // (the statement form). `opt_OBLOCKSEP` (`pars.fsy:4412`): inside a
        // computation expression FCS emits a `Virtual::BlockSep` before the
        // `->` when it sits on a continuation line (`seq { for x in xs⏎ -> x }`).
        // Drain that separator (as a layout `ERROR`, the seq-block/tuple
        // convention) when it guards a `->`, so the arrow path still fires. (A
        // `BlockSep` before `do` is an FCS parse error — not special-cased; it
        // falls through to the `do` path's "expected `do`" recovery.)
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
        ) && matches!(
            self.filtered_tokens.get(self.pos + 1),
            Some((Ok(FilteredToken::Raw(Token::RArrow)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::RArrow)), _))
        ) {
            self.parse_for_arrow_body();
        } else {
            self.parse_do_block_body("for");
        }

        self.builder.finish_node(); // FOR_EACH_EXPR
    }

    /// The comprehension body `-> e` of a `for pat in e -> body`
    /// (`pars.fsy:4412`, `arrowThenExprR` `pars.fsy:5608`). FCS desugars the
    /// arrow to `SynExpr.YieldOrReturn((true, false), e)` — an implicit
    /// `yield` — and sets the enclosing `ForEach`'s `seqExprOnly = true`
    /// (the normaliser elides `seqExprOnly`; the yield-wrapped body is what
    /// distinguishes this from the `do` form, whose body is the raw expression).
    ///
    /// Emits the body as a `YIELD_OR_RETURN_EXPR > [RARROW_TOK, <body>]` child
    /// (no `yield` keyword — [`crate::syntax::YieldExpr::is_yield`] reads the
    /// arrow), then drains the one-sided SeqBlock's trailing
    /// `Virtual::RightBlockEnd` exactly once (zero-width `ERROR`, no `bump_into`
    /// — the same careful single-virtual drain `fun`/`match` use, so an
    /// enclosing context's closers survive). FCS emits no `End` after this
    /// loop's `RightBlockEnd`, so none is consumed here.
    fn parse_for_arrow_body(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::YIELD_OR_RETURN_EXPR));
        // `->` — raw passthrough (LexFilter pushes the one-sided SeqBlock but
        // does not relabel the token), like the `fun`/`match` arrow.
        self.bump_into(SyntaxKind::RARROW_TOK);
        self.parse_seq_block_body("expected expression after `->`");
        self.builder.finish_node(); // YIELD_OR_RETURN_EXPR

        // Drain the one-sided SeqBlock close opened by `->`. Stamp zero-width
        // (no `bump_into`) so a surrounding closer / trailing trivia is not
        // pulled in, mirroring `parse_fun_expr`.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::RightBlockEnd)), _)),
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }
    }

    /// `for ident = identBody to/downto toBody do doBody` — `SynExpr.For`
    /// (`SyntaxTree.fsi:659`, `pars.fsy:4418`). The loop variable is a bare
    /// `IDENT_TOK` (FCS parses a `parenPattern` then `idOfPat`-extracts the
    /// ident; for valid input the pattern is a plain ident), `=` an `EQUALS_TOK`,
    /// the direction keyword a raw `TO_TOK`/`DOWNTO_TOK`, and the `do` body the
    /// shared SeqBlock tail. Both bounds are full expressions: `identBody` stops
    /// at the raw `to`/`downto` (not consumed by any expr production) and
    /// `toBody` at the `Virtual::Do`. Caller selected this form via the
    /// `Ident =` lookahead in [`Self::parse_for_expr`].
    fn parse_for_range_loop(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::FOR_EXPR));

        // `for` keyword — plain raw passthrough.
        self.bump_into(SyntaxKind::FOR_TOK);

        // Loop variable — a bare ident or the wildcard `_` (the lookahead
        // guaranteed `<ident|_> =`). FCS's `idOfPat` maps a `_` binder to a
        // synthetic ident with `idText = "_"`, so capturing the token as
        // `IDENT_TOK` (text `"_"`) matches the FCS projection.
        self.bump_into(SyntaxKind::IDENT_TOK);

        // `=` — plain raw passthrough.
        self.bump_into(SyntaxKind::EQUALS_TOK);

        // Start bound (`identBody`). `parse_expr` stops at the raw `to`/`downto`
        // (no expr production consumes them).
        if self.peek_is_expr_start() {
            self.parse_expr();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected start bound after `=` in `for` loop".to_string(),
                span,
            });
        }

        // Direction keyword — raw `to`/`downto` (FCS's `forLoopDirection`,
        // `pars.fsy:5634`); its identity is the `SynExpr.For.direction` bool.
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::To)), _)) => self.bump_into(SyntaxKind::TO_TOK),
            Some((Ok(FilteredToken::Raw(Token::DownTo)), _)) => {
                self.bump_into(SyntaxKind::DOWNTO_TOK)
            }
            _ => {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `to` or `downto` in `for` loop".to_string(),
                    span,
                });
            }
        }

        // End bound (`toBody`). `parse_expr` stops at the `Virtual::Do`.
        if self.peek_is_expr_start() {
            self.parse_expr();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected end bound after `to`/`downto` in `for` loop".to_string(),
                span,
            });
        }

        self.parse_do_block_body("for");

        self.builder.finish_node(); // FOR_EXPR
    }

    /// Claim the `Virtual::Do` keyword (→ [`SyntaxKind::DO_TOK`]) and parse the
    /// SeqBlock body of a `do`-block loop (`while`/`while!`/`for`).
    /// `Virtual::Do` is LexFilter's `ODO` relabel of the raw `Token::Do` at the
    /// same span — emit `DO_TOK` and advance both cursors (the `WITH_TOK`
    /// pattern). On a missing `do`, push a diagnostic and return without
    /// consuming a body; the caller's enclosing `finish_node` runs regardless.
    fn parse_do_block_body(&mut self, keyword: &str) {
        let Some((Ok(FilteredToken::Virtual(Virtual::Do)), do_span)) = self.peek().cloned() else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: format!("expected `do` in `{keyword}` loop"),
                span,
            });
            return;
        };
        self.drain_raw_up_to(do_span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::Do)), s)) if *s == do_span,
            ),
            "Virtual::Do must be backed by a raw Token::Do at raw_pos with matching span"
        );
        self.emit_text(SyntaxKind::DO_TOK, do_span);
        self.raw_pos += 1;
        self.pos += 1;

        self.parse_block_body_after_keyword(keyword);
    }

    /// Parse a *regular* offside block body (`Virtual::BlockBegin` … body …
    /// `Virtual::BlockEnd`, then a trailing `Virtual::DeclEnd`) sitting after a
    /// keyword the caller has already emitted — the `do`-body of a
    /// `while`/`for` loop (via [`Self::parse_do_block_body`]) and the
    /// finally-body of a `try … finally …` ([`Self::parse_try_expr`]), which
    /// share this scaffolding byte-for-byte. The `BlockBegin` is consumed as a
    /// zero-width ERROR, the body parsed via [`Self::parse_if_body`] (a
    /// multi-statement body wraps in [`SyntaxKind::SEQUENTIAL_EXPR`]), and the
    /// trailing `DeclEnd` consumed by [`Self::consume_block_decl_end`] (which
    /// leaves the *enclosing* block's `DeclEnd` for that block's owner). `keyword`
    /// is used only in the missing-body diagnostic.
    fn parse_block_body_after_keyword(&mut self, keyword: &str) {
        let opened = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)),
        );
        if opened {
            self.bump_into(SyntaxKind::ERROR);
        }
        self.parse_if_body(keyword, opened);
        self.consume_block_decl_end();
    }

    /// Consume a trailing `Virtual::DeclEnd` that closes an offside block body
    /// (`while … do …`, `do! …`). Two shapes:
    ///
    /// * **Explicit `done` terminator** (verbose `while c do f done`): LexFilter
    ///   relabels the raw `Token::Done` to this DeclEnd at the `done`'s span,
    ///   keeping the raw token at `raw_pos`. Claim it as [`SyntaxKind::DONE_TOK`]
    ///   — drain leading trivia, emit the text, advance both cursors — so
    ///   `text(tree) == source` and the raw `done` isn't reported as an
    ///   unsupported leftover. (The `done` keyword is elided by the normaliser;
    ///   FCS stores no `done` trivia on `While`/`DoBang`.)
    /// * **Synthetic offside close** (no `done`): the DeclEnd is zero-width with
    ///   no backing raw token — consume it as a zero-width `ERROR`, leaving
    ///   `raw_pos` put so a swallowed enclosing closer (e.g. a CE `}`) still
    ///   reaches [`Parser::bump_swallowed_closer`].
    ///
    /// No-op if the cursor is not at a `Virtual::DeclEnd`.
    pub(super) fn consume_block_decl_end(&mut self) {
        let Some((Ok(FilteredToken::Virtual(Virtual::DeclEnd)), de_span)) = self.peek().cloned()
        else {
            return;
        };
        // A `done`-backed DeclEnd shares the `done`'s span; a nested loop's own
        // `done` must match *this* DeclEnd's span (so `while a do while b do f
        // done done` claims one `done` per loop). A synthetic DeclEnd's next raw
        // token is either nothing or a following sibling/closer, never a
        // span-aligned `Token::Done`.
        let backed_by_done = matches!(
            self.next_non_trivia_raw_at_pos_with_span(),
            Some((Token::Done, s)) if s == de_span,
        );
        if backed_by_done {
            self.drain_raw_up_to(de_span.start);
            self.emit_text(SyntaxKind::DONE_TOK, de_span);
            self.raw_pos += 1;
        } else {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
        }
        self.pos += 1;
    }
}
