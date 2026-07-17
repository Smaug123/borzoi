//! Expression core: the entry points and Pratt/precedence-climbing engine
//! (`parse_expr`, `parse_pratt_expr`), the block/RHS-driving helpers
//! (`parse_let_equals_rhs`, `drain_let_rhs_block`, `parse_if_body`), and the
//! `peek_*` expr-start predicates other productions branch on. The rest of
//! the expression grammar lives in the sibling `expr_control` / `expr_op` /
//! `expr_app` / `expr_atom` modules (each its own `impl Parser` block).

use super::*;

impl<'src> Parser<'src> {
    /// Consume `EQUALS OBLOCKBEGIN <expr>` after a successfully-parsed
    /// binding LHS. Each missing piece records a `ParseError` and stops the
    /// binding cleanly so the outer impl-file loop can resume.
    ///
    /// `allow_static_optimization` admits FSharp.Core's trailing
    /// `when <conds> = <branch>` static-optimization clauses — FCS's
    /// `typedExprWithStaticOptimizationsBlock`, used *only* by `localBinding`
    /// (`let`/`use`, `pars.fsy:3327`). It is `false` for the shared callers that
    /// are *not* local bindings — member methods/constructors/`val`/auto-property
    /// (`memberCore`/`NEW`/`VAL`) and computation-expression binders (`let!`/
    /// `use!`, `ceBindingCore`), all of which take a plain
    /// `typedSequentialExprBlock` — so a trailing `when` there stays the parse
    /// error FCS reports, rather than a divergent `STATIC_OPTIMIZATION_EXPR`.
    ///
    /// Returns whether the `=` opened an offside RHS block (an `OBLOCKBEGIN` was
    /// consumed). LexFilter opens one for almost every binding RHS, but *not*,
    /// e.g., for an unparenthesised explicit constructor (`new a = …`); a caller
    /// whose terminator handling turns on "did the item leave an RHS-close
    /// `OBLOCKEND`" (see [`Self::consume_object_model_item_terminator`]) must use
    /// this rather than assume a block. Callers that always open a block (regular
    /// members, `let` bindings) ignore it.
    pub(super) fn parse_let_equals_rhs(&mut self, allow_static_optimization: bool) -> bool {
        // Expect `=`.
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Raw(Token::Equals)), _)) => {
                self.bump_into(SyntaxKind::EQUALS_TOK);
            }
            other => {
                let span = other
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `=` after binding pattern".to_string(),
                    span,
                });
                return false;
            }
        }

        // Expect `OBLOCKBEGIN` — LexFilter inserts this between `=` and the
        // RHS for offside bindings. Consume it as a zero-width ERROR token so
        // the tree stays lossless without inventing a dedicated kind. Track
        // whether we opened the offside block: any further RHS-block tokens
        // must stay inside the binding (rather than escape to the impl-file
        // loop) until the matching `OBLOCKEND`.
        let opened_block = if let Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)) =
            self.peek().cloned()
        {
            self.bump_into(SyntaxKind::ERROR);
            true
        } else {
            false
        };

        // RHS — the binding's expression, parsed as a one-sided SeqBlock so a
        // multi-statement body is a `SynExpr.Sequential`. The offside block the
        // `=` opened (`OBLOCKBEGIN`) holds statements separated by
        // `Virtual::BlockSep`; the explicit `let x = a; b` form separates with
        // `;`. Both go through the shared gatherer (see
        // [`Self::parse_seq_block_body`]), which wraps two-or-more statements in
        // `SEQUENTIAL_EXPR` and stops at the closing `Virtual::BlockEnd`.
        // Checkpoint first so a trailing FSharp.Core static-optimization run can
        // wrap the main expression and its `when` clauses in one node.
        let rhs_cp = self.builder.checkpoint();
        self.parse_seq_block_body("expected expression after `=`");

        // FSharp.Core static-optimization clauses — `mainExpr when 'T : ty =
        // branch …` (`typedExprWithStaticOptimizations`, `pars.fsy:3391`). The
        // whole RHS lives in the single offside block the `=` opened; the main
        // expression's gatherer consumes the offside separator before the first
        // `when` and stops there (a `when` is not an expression start), so the
        // cursor sits on it. Wrap the main expression and the clauses in a
        // `STATIC_OPTIMIZATION_EXPR` (the binding's RHS becomes that node). Only
        // a `localBinding` admits this; for every other caller a `when` here is
        // left for the recovery drain below (FCS's parse error).
        if allow_static_optimization
            && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::When)), _)))
        {
            self.parse_static_optimization_clauses(rhs_cp);
        }

        // Recovery drain. The gatherer has consumed every well-formed statement
        // and separator, so this only fires for malformed leftovers inside the
        // block (e.g. a stray token at a deeper indent that isn't a valid
        // statement start). It stops *before* the matching `OBLOCKEND`, leaving
        // it for the impl-file loop. Without it, such leftovers would escape via
        // the loop's `Virtual::BlockSep` fallthrough and be mis-accepted as a
        // fresh top-level decl.
        if opened_block {
            self.drain_let_rhs_block();
        }

        opened_block
    }

    /// Wrap the already-parsed main RHS expression (under `cp`) together with its
    /// trailing `when <conditions> = <branch>` static-optimization clauses in a
    /// [`SyntaxKind::STATIC_OPTIMIZATION_EXPR`] (FCS's
    /// `typedExprWithStaticOptimizations`, `pars.fsy:3391`). The caller has
    /// verified the cursor is on the first `when`. Each clause's branch is a
    /// `typedSequentialExprBlock`, parsed by [`Self::parse_seq_block_body`],
    /// which consumes the offside separator before the next `when` (or stops at
    /// the binding block's `OBLOCKEND`), so the loop simply re-checks for `when`.
    ///
    /// FCS folds the clauses *right* into nested
    /// `SynExpr.LibraryOnlyStaticOptimization` (`SyntaxTreeOps.mkSynBindingRhs`);
    /// we keep the surface flat shape `[<main-expr>, STATIC_OPT_WHEN_CLAUSE+]`
    /// and reproduce the nesting in the normaliser.
    fn parse_static_optimization_clauses(&mut self, cp: rowan::Checkpoint) {
        self.builder.start_node_at(
            cp,
            FSharpLang::kind_to_raw(SyntaxKind::STATIC_OPTIMIZATION_EXPR),
        );
        while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::When)), _))) {
            self.parse_static_optimization_clause();
        }
        self.builder.finish_node(); // STATIC_OPTIMIZATION_EXPR
    }

    /// Parse one `when <conditions> = <branch>` static-optimization clause
    /// (`staticOptimization`, `pars.fsy:3402`) into a
    /// [`SyntaxKind::STATIC_OPT_WHEN_CLAUSE`]. The caller has verified the cursor
    /// is on `when`. The `and`-chained conditions are
    /// [`Self::parse_static_opt_condition`]s; the branch is a
    /// `typedSequentialExprBlock` (so a multi-statement / multi-line body works).
    fn parse_static_optimization_clause(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::STATIC_OPT_WHEN_CLAUSE));
        self.bump_into(SyntaxKind::WHEN_TOK);
        self.parse_static_opt_condition();
        while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::And)), _))) {
            self.bump_into(SyntaxKind::AND_TOK);
            self.parse_static_opt_condition();
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Equals)), _))
        ) {
            self.bump_into(SyntaxKind::EQUALS_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `=` after static-optimization conditions".to_string(),
                span,
            });
        }
        self.parse_seq_block_body("expected a branch expression after `=` in a `when` clause");
        self.builder.finish_node(); // STATIC_OPT_WHEN_CLAUSE
    }

    /// Parse one static-optimization condition
    /// (`staticOptimizationCondition`, `pars.fsy:3413`) into a
    /// [`SyntaxKind::STATIC_OPT_CONDITION`]: the subject typar (`'T`/`^T`, reusing
    /// [`Self::parse_typar_decl`]) followed by either `: <type>` (`'T : ty`,
    /// `WhenTyparTyconEqualsTycon`) or the bare `struct` (`'T struct`,
    /// `WhenTyparIsStruct`). Anything else is a clean recovery error.
    fn parse_static_opt_condition(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::STATIC_OPT_CONDITION));
        self.parse_typar_decl(false);
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Colon)), _)) => {
                self.bump_into(SyntaxKind::COLON_TOK);
                self.parse_type();
            }
            Some((Ok(FilteredToken::Raw(Token::Struct)), _)) => {
                self.bump_into(SyntaxKind::STRUCT_TOK);
            }
            other => {
                let span = other
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `:` or `struct` in a static-optimization condition"
                        .to_string(),
                    span,
                });
            }
        }
        self.builder.finish_node(); // STATIC_OPT_CONDITION
    }

    /// Parse the value side of an `<-` assignment — FCS's `declExprBlock`
    /// (`pars.fsy:4063`, the RHS of `minusExpr LARROW declExprBlock`).
    /// `declExprBlock` has two arms, and which one applies is decided by the
    /// LexFilter: the LARROW arm (`LexFilter.fs:2318`) opens an offside
    /// `OBLOCKBEGIN`…`OBLOCKEND` block *only* when the RHS is on a new line or
    /// starts with a control-flow keyword (`x <-⏎  e`, `x <- if …`).
    ///
    /// * **Block present** (`OBLOCKBEGIN` arm): gather a one-sided SeqBlock so
    ///   a multi-statement RHS becomes a `SynExpr.Sequential`, then **consume**
    ///   the matching `OBLOCKEND`. The assignment is a *nested expression*, so
    ///   it owns its close — this mirrors [`Self::parse_if_body`], **not**
    ///   [`Self::parse_let_equals_rhs`] (whose decl-level `OBLOCKEND` is left
    ///   for the impl-file loop). Leaving it unconsumed lets an enclosing block
    ///   — a function body, a paren — mistake the RHS's close for its own, so
    ///   statements after the assignment fall out of scope
    ///   (`let f () =⏎  x <-⏎    1⏎  y` would drop `y` from `f`'s body).
    /// * **No block** (same-line `x <- 1`): the `declExpr` arm — a *single*
    ///   expression (tuple-inclusive, so `x <- a, b` works). Crucially this is
    ///   **not** [`Self::parse_seq_block_body`]: unlike a `let` RHS (whose `=`
    ///   always opens an `AddBlockEnd` block that fences off the next decl), a
    ///   same-line `<-` has no fence, so sequencing here would wrongly swallow
    ///   the following top-level statement (`x <- 1⏎ y <- 2`).
    fn parse_assign_rhs(&mut self) {
        if let Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)) = self.peek().cloned() {
            self.bump_into(SyntaxKind::ERROR);
            self.parse_seq_block_body("expected expression after `<-`");
            self.drain_and_consume_offside_block_end("unexpected token in assignment value");
        } else if self.peek_is_expr_start() && !self.at_swallowed_seq_closer() {
            self.parse_expr();
        } else {
            // The `!at_swallowed_seq_closer` guard matches the `<-` firing gate
            // in [`Self::parse_pratt_expr`]: inside `( … )` the `)` is stripped
            // from the filtered stream, so on a missing RHS (`(x <- )..3`) a
            // bare `peek_is_expr_start` would see the token *past* the closer,
            // recurse across it, and (for a `..` successor) reach
            // `parse_const_payload`'s `unreachable!`. Treating the RHS as
            // missing here records a recovery error and leaves the `)` for the
            // enclosing paren — no panic, fully lossless.
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected expression after `<-`".to_string(),
                span,
            });
        }
    }

    /// Drain tokens belonging to a let-binding's offside RHS block, stopping
    /// *before* the matching `Virtual::BlockEnd` so the impl-file loop's
    /// virtual-fallthrough arm consumes it (and its drained trailing
    /// trivia lands at module level, matching how `parse_module_decl`
    /// keeps inter-decl trivia outside the decl). Each non-trivia,
    /// non-virtual token in the block records a single "unexpected token
    /// after binding expression" error and is bumped as ERROR; intervening
    /// `Virtual::BlockSep` virtuals are consumed silently.
    pub(super) fn drain_let_rhs_block(&mut self) {
        let mut reported = false;
        while let Some((res, span)) = self.peek().cloned() {
            match &res {
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) => return,
                Ok(FilteredToken::Virtual(_)) => {
                    self.bump_into(SyntaxKind::ERROR);
                }
                _ => {
                    if !reported {
                        self.errors.push(ParseError {
                            message: "unexpected token after binding expression".to_string(),
                            span,
                        });
                        reported = true;
                    }
                    self.bump_into(SyntaxKind::ERROR);
                }
            }
        }
    }

    /// Close a *nested-expression* offside block: after
    /// [`Self::parse_seq_block_body`] has gathered the body, drain any
    /// malformed leftovers (depth-tracking nested `BlockBegin`/`BlockEnd` pairs
    /// so a sub-block's close isn't mistaken for ours) and then consume the
    /// matching `Virtual::BlockEnd`.
    ///
    /// The BlockEnd is consumed as a zero-width ERROR with a manual cursor bump
    /// (not `bump_into`) because its span often coincides with a
    /// lexfilter-swallowed `)` — advancing the raw cursor would steal that `)`
    /// from [`Self::parse_paren_expr`]'s `bump_swallowed_rparen`. `unexpected`
    /// is the diagnostic for a stray non-virtual token before the close.
    ///
    /// Contrast [`Self::drain_let_rhs_block`], which stops *before* the
    /// BlockEnd: a decl-level `let` RHS leaves its close for the impl-file
    /// loop, but a nested expression (if/else branch, `<-` value) owns its own.
    fn drain_and_consume_offside_block_end(&mut self, unexpected: &str) {
        let mut depth: u32 = 0;
        let mut reported = false;
        while let Some((res, span)) = self.peek().cloned() {
            match &res {
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) if depth == 0 => break,
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) => {
                    depth -= 1;
                    self.bump_into(SyntaxKind::ERROR);
                }
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => {
                    depth += 1;
                    self.bump_into(SyntaxKind::ERROR);
                }
                Ok(FilteredToken::Virtual(_)) => {
                    self.bump_into(SyntaxKind::ERROR);
                }
                _ => {
                    if !reported {
                        self.errors.push(ParseError {
                            message: unexpected.to_string(),
                            span,
                        });
                        reported = true;
                    }
                    self.bump_into(SyntaxKind::ERROR);
                }
            }
        }

        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _)),
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }
    }

    /// Gather a one-sided offside `SeqBlock` body: parse the first statement,
    /// then while the next filtered token(s) are statement separators followed
    /// by an expression start, consume the separator(s) and parse the next
    /// statement. Two separator forms are accepted, mirroring FCS's `seps`
    /// (`pars.fsy:6981`, `(OBLOCKSEP | SEMICOLON)+`):
    ///
    /// * a same-indent offside `Virtual::BlockSep` — emitted as a zero-width
    ///   ERROR (the raw newline stays in trivia, owned by the preceding
    ///   statement), and
    /// * an explicit `;` (raw [`Token::Semi`]) — emitted as a real
    ///   [`SyntaxKind::SEMI_TOK`].
    ///
    /// A *maximal run* of separators is consumed before each statement, so the
    /// `;`-then-newline combo (`a;⏎ b`) collapses to one boundary. `;;`
    /// ([`Token::SemiSemi`], the top-level terminator) is a distinct token and
    /// is **not** a separator — the gather stops at it.
    ///
    /// When more than one statement is gathered, wrap the run in
    /// [`SyntaxKind::SEQUENTIAL_EXPR`] spanning the whole body; a single
    /// statement is left as a bare `Expr` child of the surrounding node. The
    /// flat `SEQUENTIAL_EXPR` projects to FCS's right-leaning binary
    /// `SynExpr.Sequential(_, _, e1, Sequential(_, _, e2, e3, …), …)` via
    /// [`crate::syntax::SequentialExpr::statements`] (separators are tokens,
    /// filtered out); the diff normaliser ignores `isTrueSeq` / the debug
    /// point, so the flat shape diff-matches.
    ///
    /// `missing_first` is the diagnostic pushed when the body's *first*
    /// statement is absent. Returns the statement count (0 when empty). The
    /// caller owns the trailing block-close scaffolding (the
    /// `Virtual::BlockEnd` / `RightBlockEnd` + `End` drain differs per
    /// context), so this helper stops at the first non-separator,
    /// non-expression token.
    pub(super) fn parse_seq_block_body(&mut self, missing_first: &str) -> u32 {
        self.parse_seq_block_body_impl(missing_first, true)
    }

    /// `sequentialExpr` — like [`Self::parse_seq_block_body`] but **without** the
    /// trailing `: T` annotation. FCS's `listExprElements` / `arrayExprElements`
    /// (`pars.fsy`) are a bare `sequentialExpr`, not a `typedSequentialExpr`, so a
    /// bracketed `[1 : int]` / `[|1 : int|]` is a parse error (the annotation
    /// needs parens). Used only by the list/array element path; every other body
    /// position is a `typedSequentialExpr` and uses [`Self::parse_seq_block_body`].
    pub(super) fn parse_seq_block_elements(&mut self, missing_first: &str) -> u32 {
        self.parse_seq_block_body_impl(missing_first, false)
    }

    /// Shared implementation of [`Self::parse_seq_block_body`] (a
    /// `typedSequentialExpr`, `allow_typed = true`) and
    /// [`Self::parse_seq_block_elements`] (a `sequentialExpr`, `allow_typed =
    /// false`). The only difference is whether a trailing `: T` annotation is
    /// absorbed into a [`SyntaxKind::TYPED_EXPR`].
    fn parse_seq_block_body_impl(&mut self, missing_first: &str, allow_typed: bool) -> u32 {
        let cp = self.builder.checkpoint();
        let mut count: u32 = 0;
        if self.peek_is_expr_start() {
            self.parse_expr();
            count = 1;
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: missing_first.to_string(),
                span,
            });
        }
        self.finish_seq_block(cp, count, allow_typed)
    }

    /// Continue a statement sequence whose first element (if any) has already
    /// been parsed under `cp` (with `count` elements so far): consume
    /// separator-introduced statements (`;` / offside `OBLOCKSEP`, stopping at a
    /// LexFilter-swallowed closer), wrap the run in a [`SyntaxKind::SEQUENTIAL_EXPR`]
    /// once it holds more than one element, and — when `allow_typed` — bind a
    /// trailing `: T` annotation as a [`SyntaxKind::TYPED_EXPR`]. Returns the final
    /// element count.
    ///
    /// Split out of [`Self::parse_seq_block_body_impl`] so the `new`-headed
    /// computation-expression arm ([`Self::parse_obj_or_computation_brace`]) can
    /// reuse the exact separator / swallowed-closer / wrapping discipline: there
    /// the first body expression is the `new T(args)` base call, parsed separately
    /// before the object-vs-computation decision, after which the remaining
    /// statements are gathered here (`allow_typed = false`, the `computationExpr`
    /// `sequentialExpr` production).
    pub(super) fn finish_seq_block(
        &mut self,
        cp: rowan::Checkpoint,
        mut count: u32,
        allow_typed: bool,
    ) -> u32 {
        // Count of `then` RHS offside blocks opened (see the `Virtual::Then`
        // separator arm); their matching `OBLOCKEND`s are drained after the loop.
        let mut then_blocks = 0u32;
        loop {
            // Consume a maximal run of statement separators — but never one
            // that sits *past* a LexFilter-swallowed closer (`)` of a paren
            // expr, `}` of a computation expr). When a parenthesised body is
            // followed by an outer same-indent statement
            // (`let x =⏎  (a⏎  )⏎  b`), LexFilter emits the *outer* `BlockSep`
            // (between the paren expr and `b`) while the swallowed `)` is still
            // pending on the raw stream; that separator belongs to the
            // enclosing sequence, not this one. The closer is absent from the
            // filtered peek, so gate on the raw stream before each separator —
            // see [`Self::at_swallowed_seq_closer`].
            let mut consumed_sep = false;
            loop {
                if self.at_swallowed_seq_closer() {
                    break;
                }
                match self.peek() {
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _)) => {
                        self.bump_into(SyntaxKind::ERROR);
                        consumed_sep = true;
                    }
                    Some((Ok(FilteredToken::Raw(Token::Semi)), _)) => {
                        self.bump_into(SyntaxKind::SEMI_TOK);
                        consumed_sep = true;
                    }
                    // `expr then expr` — FCS's `declExpr OTHEN OBLOCKBEGIN
                    // typedSequentialExpr oblockend` (`pars.fsy:4118`), a
                    // `SynExpr.Sequential(isTrueSeq = false, …)` used by secondary
                    // constructors (`T(x) then this.P <- 1`) and any statement
                    // position. LexFilter relabels `then` to `Virtual::Then`
                    // (backed by a real `Token::Then`) and wraps the RHS in its own
                    // offside block. Emit the `then` as a `THEN_TOK` (the
                    // `parse_do_bang` idiom), consume the RHS block's `OBLOCKBEGIN`
                    // zero-width, and record the open block so its matching
                    // `OBLOCKEND` is drained after the RHS statements — the RHS
                    // statements then join *this* flat sequence, matching the
                    // normaliser's flattening of FCS's right-leaning `Sequential`.
                    Some((Ok(FilteredToken::Virtual(Virtual::Then)), then_span)) => {
                        let then_span = then_span.clone();
                        self.drain_raw_up_to(then_span.start);
                        self.emit_text(SyntaxKind::THEN_TOK, then_span);
                        self.raw_pos += 1;
                        self.pos += 1;
                        if matches!(
                            self.peek(),
                            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _))
                        ) {
                            self.bump_into(SyntaxKind::ERROR);
                            then_blocks += 1;
                        }
                        consumed_sep = true;
                    }
                    _ => break,
                }
            }
            if !consumed_sep {
                break;
            }
            // A *trailing* separator immediately before a swallowed closer
            // (`(a;) + b` → the `;`) must not continue: FCS's `declExpr seps`
            // arm treats it as trailing and the next token belongs to the
            // enclosing construct. `peek_is_expr_start()` alone would wrongly
            // fire on the token *past* the swallowed closer (the `+`), dragging
            // the real `)` into the tree as ERROR — so re-check the raw stream
            // now that the separator run is consumed.
            if self.at_swallowed_seq_closer() {
                break;
            }
            if self.peek_is_expr_start() {
                self.parse_expr();
                count += 1;
            } else {
                break;
            }
        }
        // Drain the `OBLOCKEND`s of any `then` RHS blocks opened above (zero-width
        // ERRORs, advancing only `pos`), so the caller sees just the enclosing
        // block's own close virtuals — its statements have already joined this
        // flat sequence.
        for _ in 0..then_blocks {
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
            ) {
                self.builder
                    .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                self.pos += 1;
            }
        }
        if count > 1 {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::SEQUENTIAL_EXPR));
            self.builder.finish_node();
        }
        // `typedSequentialExpr: sequentialExpr COLON typ` (FCS `pars.fsy:4088`):
        // an optional trailing `: T` binds the *whole* sequence, yielding
        // `SynExpr.Typed`. Every `parse_seq_block_body` caller is a
        // `typedSequentialExpr` position (a lambda / `if` / `try` / paren body,
        // a `match`-clause result), so the annotation belongs here — wrapping the
        // gatherer's single expr or `SEQUENTIAL_EXPR` (`(a; b : int)` is
        // `Typed(Sequential(a, b), int)`). It is, in particular, *inside* the
        // lambda body: `fun x -> y : int` is `Lambda(Typed(y, int))`, not
        // `Typed(Lambda(x, y), int)`.
        //
        // The `: T` is bound here only when a real annotation colon genuinely
        // follows this body — see [`Self::at_typed_annotation_colon`] for the
        // dual filtered/raw gate (it rejects both the outer `(e) : T` and an
        // offside annotation parked behind this body's close-virtuals). Skipped
        // entirely for a `sequentialExpr` position (`allow_typed = false`, the
        // list/array element path), where FCS has no annotation production.
        if allow_typed && count >= 1 && self.at_typed_annotation_colon() {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TYPED_EXPR));
            self.bump_into(SyntaxKind::COLON_TOK);
            self.parse_type();
            self.builder.finish_node();
        }
        count
    }

    /// `true` when a real `: T` annotation colon is the immediate next token —
    /// the discriminator for the `typedSequentialExpr: sequentialExpr COLON typ`
    /// body wrap and the `YIELD declExpr COLON typ` yield wrap.
    ///
    /// Both a filtered and a raw check are required, because each alone
    /// misfires in a layout the other handles:
    /// * **filtered** `peek() == Token::Colon` — the colon must be the genuine
    ///   next filtered token, *not* a pending layout close-virtual
    ///   (`ORIGHT_BLOCK_END` / `OEND` / `OffsideBlockSep`). Without this, an
    ///   offside outer annotation (`fun x ->⏎ x⏎ : int`, the `:` belonging to the
    ///   enclosing body) would consume this body's close-virtual as the colon.
    /// * **raw** `next_non_trivia_raw_at_pos() == Token::Colon` — no
    ///   LexFilter-swallowed `)` sits before it. The swallowed `)` is gone from
    ///   the *filtered* stream, so for an outer `(e) : T` the filtered peek is the
    ///   colon even though the `:` is past the `)`; the raw stream still has the
    ///   `)` first, so this check declines (the annotation is the enclosing
    ///   expression's, not this paren body's).
    pub(super) fn at_typed_annotation_colon(&self) -> bool {
        matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _)))
            && self
                .next_non_trivia_raw_at_pos()
                .is_some_and(|t| matches!(t, Token::Colon))
    }

    /// `true` when the next non-trivia *raw* token is a LexFilter-swallowed
    /// sequence closer — the `)` of a paren expression or the `}` of a
    /// computation expression. Both are removed from the *filtered* stream
    /// (see [`Self::bump_swallowed_closer`]) but survive on the raw stream
    /// past `raw_consumed_end`, so [`Self::next_non_trivia_raw_at_pos`]
    /// surfaces them even though `peek()` cannot.
    ///
    /// [`Self::parse_seq_block_body`] consults this both before consuming a
    /// separator (so an *outer* `Virtual::BlockSep` emitted past the swallowed
    /// closer — `(a⏎)⏎b` — isn't pulled into this body) and after a separator
    /// run (so a trailing separator — `(a;) + b` — doesn't sequence the
    /// enclosing construct's tokens). A legitimate next statement never begins
    /// with a closer, so this never stops a genuine continuation. Mirrors the
    /// raw-stream gate [`Self::at_tuple_continuation`] uses for the tuple `,`.
    pub(super) fn at_swallowed_seq_closer(&self) -> bool {
        matches!(
            self.next_non_trivia_raw_at_pos(),
            Some(Token::RParen | Token::RBrace)
        )
    }

    /// Parse the body of a then- or else-branch as an offside `SeqBlock`.
    /// FCS's grammar lets either branch be a `SynExpr.Sequential` — a
    /// flat list of statements separated by `Virtual::BlockSep` virtuals
    /// inside a `Virtual::BlockBegin` / `Virtual::BlockEnd` pair.
    ///
    /// Caller must have already consumed the leading [`Virtual::BlockBegin`]
    /// (if any) as a zero-width ERROR; pass `has_block = true` to signal
    /// that. With `has_block = false` we parse a single expression and
    /// return without touching block-virtuals — this covers defensive
    /// fallback paths where LexFilter didn't emit a BlockBegin.
    ///
    /// Shape produced:
    /// - **Single expression** → one direct child of the surrounding
    ///   [`SyntaxKind::IF_THEN_ELSE_EXPR`] (e.g. `if c then 1 else 2`).
    /// - **Two or more** → wrapped in [`SyntaxKind::SEQUENTIAL_EXPR`].
    ///   The BlockSep virtuals between statements are stamped as
    ///   zero-width ERROR placeholders (raw newlines stay in trivia,
    ///   owned by the preceding statement).
    ///
    /// Recovery: anything between the last successfully-parsed statement
    /// and the matching BlockEnd is drained as ERROR with a single
    /// diagnostic. The drain tracks nested `BlockBegin`/`BlockEnd` pairs
    /// (depth counter) so a sub-block's BlockEnd doesn't terminate the
    /// outer drain — note however that under normal control flow the
    /// recursive `parse_expr` calls consume their own nested blocks, so
    /// the recovery drain only fires on malformed input.
    ///
    /// **Why advance past BlockEnd rather than stop before it (as
    /// [`Self::drain_let_rhs_block`] does):** the if expression is followed
    /// by Pratt infix continuations like `(if … else 2) + 3`. Leaving the
    /// cursor on a `Virtual::BlockEnd` blocks the outer Pratt loop's
    /// `peek_infix_continuation` from seeing the `+`.
    ///
    /// **Why a zero-width ERROR + manual `pos += 1` rather than
    /// `bump_into(BLOCK_END_TOK)`:** the trailing BlockEnd's span often
    /// coincides with a lexfilter-swallowed `)` (the BlockEnd is emitted
    /// at the byte where the `)` lives but the `)` itself was stripped
    /// from the filtered stream). `bump_into` would call
    /// `drain_raw_up_to(next_filtered.start)`, advancing the raw cursor
    /// past the `)` and stamping it as an ERROR — stealing it from
    /// `parse_paren_expr`'s `bump_swallowed_rparen`. The zero-width
    /// placeholder advances the filtered cursor while raw stays put.
    ///
    /// **Why no `drain_raw_up_to` before emitting the placeholder:** raw
    /// trivia (newlines, comments) sitting between the body and BlockEnd
    /// belongs to the enclosing scope — see [`Self::drain_let_rhs_block`]'s
    /// top comment about leaving inter-decl trivia for the impl-file loop
    /// to drain at module level. Pulling it into the if expression
    /// misanchors LSP ancestor queries (`// c` between two `let`s would
    /// land inside the prior `let`).
    pub(super) fn parse_if_body(&mut self, branch: &str, has_block: bool) {
        // Gather the (possibly multi-statement) branch body. When there is no
        // offside block (`has_block == false`, the single-line `then`/`else`
        // form) no `BlockSep` follows, so the gatherer parses exactly one
        // statement — equivalent to the former single-expression path.
        self.parse_seq_block_body(&format!("expected expression after `{branch}`"));

        if !has_block {
            return;
        }

        // Recovery drain + consume the matching BlockEnd. With proper
        // multi-statement parsing the drain only fires for malformed input
        // where a token sits between the last statement and the close.
        self.drain_and_consume_offside_block_end(&format!("unexpected token in `{branch}` branch"));
    }

    /// Parse a full `declExpr` — the `:=` (`COLON_EQUALS`) ref-cell
    /// assignment level, the loosest binary operator except `<-`.
    ///
    /// `:=` is FCS's `declExpr COLON_EQUALS declExpr` (`pars.fsy:4658`),
    /// lowered by `mkSynInfix` to the *same* two-tier `App` shape as `+` /
    /// `*` (so it reuses [`SyntaxKind::INFIX_APP_EXPR`] /
    /// [`SyntaxKind::APP_EXPR`] and [`Self::emit_infix_op_as_long_ident`] —
    /// no dedicated node). Its precedence is what forces a level *above* the
    /// tuple loop rather than an entry in [`Self::peek_infix_op`]:
    ///
    /// ```text
    /// %right LARROW         (pars.fsy:343)   ← `<-`   (loosest)
    /// %right COLON_EQUALS   (pars.fsy:344)   ← `:=`
    /// %left  COMMA          (pars.fsy:346)   ← tuple
    /// %left  + * < = …                       ← infix ops (tighter)
    /// ```
    ///
    /// So both operands are tuple-inclusive (`a, b := c, d` is
    /// `App(App(:=, Tuple(a,b)), Tuple(c,d))`) and `:=` is right-associative
    /// (`a := b := c` is `App(App(:=,a), App(App(:=,b),c))`) — the latter
    /// falls out of parsing the RHS back at *this* level. `<-` is handled one
    /// frame down in [`Self::parse_pratt_expr`] (it binds a `minusExpr` LHS),
    /// so the two compose without extra wiring: `x <- y := z` is
    /// `Set(x, y := z)` (the `<-` RHS, [`Self::parse_assign_rhs`], re-enters
    /// here) and `a := b <- c` is `App(App(:=,a), Set(b,c))`.
    pub(super) fn parse_expr(&mut self) {
        let cp = self.builder.checkpoint();
        self.parse_tuple_expr();
        self.continue_assign_expr(cp);
    }

    /// The `:=` continuation of [`Self::parse_expr`] (the top precedence level),
    /// split out so the appExpr-headed brace handler
    /// ([`Self::parse_app_head_brace`]) can resume the full precedence climb from
    /// an already-parsed `appExpr` LHS at `cp` (`appExpr` is a `tupleExpr`, so
    /// `:=` is its only outer continuation). Behaviourally identical to the inline
    /// tail it replaces.
    fn continue_assign_expr(&mut self, cp: rowan::Checkpoint) {
        // `:=` binds the whole preceding tuple. The **swallowed-closer gate**
        // (`!at_swallowed_seq_closer`, the guard [`Self::parse_pratt_expr`]'s
        // `<-` block also applies) decides whether the `:=` belongs to *this*
        // frame: inside `( … )` the `)` is stripped from the filtered stream,
        // so the paren body's own `parse_expr` would otherwise see the *outer*
        // `:=` as its successor and wrongly fold it into `Paren(Set(a, 1))`.
        // The guard keeps `(a) := 1` as `App(App(:=, Paren(a)), 1)`.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::ColonEquals)), _))
        ) && !self.at_swallowed_seq_closer()
        {
            // Inner `App(isInfix=true, op, lhs)`: the already-emitted tuple
            // LHS (between `cp` and now) + `:=` as a single-segment
            // SynLongIdent (consuming the operator). Source-order children
            // `[lhs, op]`, swapped back to FCS order by the typed-AST
            // accessors — identical to the Pratt infix path.
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::INFIX_APP_EXPR));
            self.emit_infix_op_as_long_ident();
            self.builder.finish_node();
            // Outer `App(isInfix=false, inner, rhs)`. The RHS is a full
            // `declExpr` (`parse_expr` — tuple-, range- and `:=`-inclusive,
            // right-associative), so it is guarded by the *broad*
            // [`Self::peek_is_expr_start`] — the predicate that matches what
            // `parse_expr` actually accepts, exactly as
            // [`Self::parse_assign_rhs`] guards `<-`'s same-line `parse_expr`
            // RHS. (The infix-level [`Self::is_expr_start_at`] would be wrong
            // here: it is narrower than `parse_expr`'s starter set, rejecting
            // an open range `cell := ..3` or a CE `do!` body `r := do! m` that
            // FCS accepts.) The `!at_swallowed_seq_closer` half mirrors the
            // firing gate: inside `( … )` the `)` is stripped from the filtered
            // stream, so on a missing RHS (`(r := )..3`) `peek_is_expr_start`
            // would see the token *past* the closer and recurse across it —
            // draining the `)` as ERROR and (for a `..` successor) reaching
            // `parse_const_payload`'s `unreachable!`. On incomplete input
            // (`r :=`, `r := )`, `let f () = r :=`, `(r := )..3`) the `:=`
            // stays in the tree and a recovery error is recorded instead — no
            // panic, fully lossless (an LSP parser sees half-typed input
            // constantly).
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_EXPR));
            if self.peek_is_expr_start() && !self.at_swallowed_seq_closer() {
                // Depth-guarded: `:=` is right-associative, so a chain
                // (`a := a := …`) recurses through this RHS `parse_expr` *after*
                // the Pratt/minus frames for the LHS have returned and
                // decremented the counter — a tail continuation that bypasses the
                // main-cycle guards, so it needs its own.
                self.with_depth(|p| p.parse_expr());
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected expression after `:=`".to_string(),
                    span,
                });
            }
            self.builder.finish_node();
        }
    }

    /// Resume the full precedence climb from an `appExpr` already parsed under
    /// `cp` — exactly the continuation `parse_expr` would have run had it parsed
    /// that `appExpr` as its `minusExpr` head (all four climb levels checkpoint at
    /// the same coincident position, so they share `cp`). Used by
    /// [`Self::parse_app_head_brace`] for the computation-expression fallback,
    /// where the `appExpr` is the first statement's head, possibly extended by
    /// infix operators, a `..` range, a tuple `,`, or a `:=` assignment.
    pub(super) fn continue_expr_after_app(&mut self, cp: rowan::Checkpoint) {
        self.continue_pratt_expr(cp, 0, false, false);
        self.continue_range_expr(cp);
        self.continue_tuple_expr(cp);
        self.continue_assign_expr(cp);
    }

    /// Parse a `tupleExpr`. If the parsed sub-expression is followed
    /// by `,`, the sub-expression plus following comma-separated
    /// sub-expressions are wrapped in a [`SyntaxKind::TUPLE_EXPR`] using
    /// rowan's `Checkpoint` to splice the already-emitted first element
    /// underneath the tuple node.
    ///
    /// The comma binds looser than the `..` range operator and every infix
    /// operator, but tighter than `:=` / `<-` (which wrap a whole tuple — see
    /// [`Self::parse_expr`]). Each tuple element is itself parsed via
    /// [`Parser::parse_range_expr`], so the range operator and all infix
    /// operators bind tighter than the comma: `a + b, c` parses as
    /// `Tuple(App(+,a,b), c)`, and `1..3, 4..5` as
    /// `Tuple(IndexRange(1,3), IndexRange(4,5))`.
    fn parse_tuple_expr(&mut self) {
        let cp = self.builder.checkpoint();
        self.parse_range_expr();
        self.continue_tuple_expr(cp);
    }

    /// The `,` continuation of [`Self::parse_tuple_expr`], split out so the
    /// appExpr-headed brace handler can resume the climb from a pre-parsed
    /// `appExpr` LHS at `cp`. Behaviourally identical to the inline tail.
    fn continue_tuple_expr(&mut self, cp: rowan::Checkpoint) {
        if self.at_tuple_continuation() {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TUPLE_EXPR));
            while self.at_tuple_continuation() {
                self.bump_into(SyntaxKind::COMMA_TOK);
                // Multi-line layouts like `(\n  1,\n  2\n)` see LexFilter
                // emit `Virtual::BlockSep` between the comma and the next
                // element. The comma is the explicit separator; the
                // BlockSep is offside scaffolding the tuple loop should
                // step over before deciding whether an element follows.
                while matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                ) {
                    self.bump_into(SyntaxKind::ERROR);
                }
                if self.peek_is_expr_start() {
                    self.parse_range_expr();
                } else {
                    let span = self
                        .peek()
                        .map(|(_, span)| span.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message: "expected expression after `,` in tuple".to_string(),
                        span,
                    });
                    break;
                }
            }
            self.builder.finish_node();
        }
    }

    /// The `..` range level (`SynExpr.IndexRange`) — FCS's `declExpr DOT_DOT
    /// declExpr` family (`pars.fsy:4820-4837`). `..` binds just above `,`
    /// (`%left DOT_DOT`, `pars.fsy:348`) and below every Pratt infix operator,
    /// so it sits between [`Parser::parse_expr`]'s tuple loop and
    /// [`Parser::parse_pratt_expr`]: each bound is a full `parse_pratt_expr(0)`
    /// (so `a + b .. c * d` is `(a+b)..(c*d)`), and the whole range is one
    /// tuple element (so `1..3, 4..5` is `Tuple(IndexRange, IndexRange)`).
    ///
    /// Three shapes, mirroring FCS's productions (`pars.fsy:4820-4837`):
    /// * `lower..upper` — both bounds present;
    /// * `lower..` — open upper (the next token isn't an expression start —
    ///   `]`, `,`, a block close);
    /// * `..upper` — open lower (a leading `..`, FCS's `DOT_DOT declExpr`).
    ///
    /// Left-associative (`a..b..c` → `IndexRange(IndexRange(a,b), c)`): the
    /// loop re-wraps `[cp..now]` after each `..`. The `..^` from-end operator
    /// (`Token::DotDotHat`) is **not** handled here (deferred —
    /// `SynExpr.IndexFromEnd`); it leaves a clean error rather than a wrong tree.
    ///
    /// The whole-dimension wildcard `*` (FCS's nullary `STAR` production) is
    /// **not** handled at this level — it is a high-precedence *atom*
    /// ([`Parser::parse_index_wildcard`], reached via `parse_pratt_expr`'s atom
    /// head), so it flows through the lower-bound parse below and can be an
    /// infix LHS (`* + 1`) or a range bound (`* .. 3`, `1 .. *`).
    pub(super) fn parse_range_expr(&mut self) {
        let cp = self.builder.checkpoint();

        // Open-lower `..upper`: a leading `..` with no left operand. FCS's
        // `DOT_DOT declExpr`. Delegated to [`Self::parse_open_lower_range`] so
        // the `lazy` operand (whose `declExpr` operand also admits a leading
        // open-lower range — `lazy ..3`) can reuse the exact same production.
        if self.at_range_op() {
            self.parse_open_lower_range();
            return;
        }

        self.parse_pratt_expr(0);
        self.continue_range_expr(cp);
    }

    /// The `..` continuation of [`Self::parse_range_expr`], split out so the
    /// appExpr-headed brace handler can resume the climb from a pre-parsed
    /// `appExpr` LHS at `cp`. Behaviourally identical to the inline tail. (The
    /// open-lower `..upper` head stays in [`Self::parse_range_expr`]: an `appExpr`
    /// LHS is never a leading `..`.)
    fn continue_range_expr(&mut self, cp: rowan::Checkpoint) {
        while self.at_range_op() {
            self.bump_into(SyntaxKind::DOT_DOT_TOK);
            // Open-upper `lower..`: no bound-starter after the `..` (e.g.
            // `arr.[2..]`, where `]` follows; or `(a..) + b`, where the next
            // filtered token sits past the swallowed `)`). Otherwise parse the
            // upper bound. This is the valid open-upper form, so — unlike the
            // leading branch — a missing upper is *not* an error. A leading `^`
            // upper (`3..^1`, or the lex-filter-split `..^1`) is the ordinary
            // from-end prefix `parse_pratt_expr` → `parse_minus_expr` handles.
            if self.at_range_op() {
                // A range upper may itself be a leading open-lower range
                // (`a.. ..b`). Keep the ordinary `a..b..c` path left-associative
                // by only recursing to the open-lower helper when the bound
                // actually starts with another `..`; non-leading uppers stay as
                // Pratt operands below.
                self.parse_open_lower_range();
            } else if self.peek_starts_range_bound() {
                self.parse_pratt_expr(0);
            }
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::INDEX_RANGE_EXPR));
            self.builder.finish_node();
        }
    }

    /// Parse a leading open-lower range `..upper` (FCS's `DOT_DOT declExpr`),
    /// emitting `INDEX_RANGE_EXPR > [DOT_DOT_TOK, <upper>]` (`IndexRange(None,
    /// Some upper)`). The caller has verified the cursor is at the `..`.
    ///
    /// FCS *requires* the upper bound — a bare `..` (both bounds absent) is the
    /// separate `*` production, so a missing upper is a syntax error (FCS
    /// reports FS0010). Report it, but still build the node for lossless
    /// recovery. The upper bound is a range-level expression: `..a .. b` is
    /// `IndexRange(None, IndexRange(a, b))`, and `.. ..3` is
    /// `IndexRange(None, IndexRange(None, 3))`. That is the one recursive case
    /// ordinary left-bounded ranges do not use for non-leading uppers, preserving
    /// `a..b..c` as left-associative.
    ///
    /// Shared by [`Self::parse_range_expr`]'s leading branch and the `lazy`
    /// operand ([`Self::parse_lazy_or_assert`]): both admit a leading open-lower
    /// range as a `declExpr` (`lazy ..3` = `Lazy(IndexRange(None, 3))`). Because
    /// the upper is parsed at the range level, `lazy ..a .. b` keeps the whole
    /// nested range under the lazy node.
    pub(super) fn parse_open_lower_range(&mut self) {
        let cp = self.builder.checkpoint();
        self.bump_into(SyntaxKind::DOT_DOT_TOK);
        // A from-end upper (`..^1`, lex-filter-split to `..` then `^1`) is still
        // handled by the recursive range parse through its Pratt operand.
        if self.peek_is_expr_start() && !self.at_swallowed_seq_closer() {
            self.with_depth(|p| p.parse_range_expr());
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected an expression after `..`".to_string(),
                span,
            });
        }
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::INDEX_RANGE_EXPR));
        self.builder.finish_node();
    }

    /// `true` when a range *bound* (the `parse_pratt_expr(0)` operand on either
    /// side of `..`) can start at the cursor. It is [`Self::peek_is_expr_start`]
    /// minus two cases that the Pratt operand cannot take:
    ///
    /// * a leading `..` ([`Self::peek_is_range_op`]) — an open-lower range is a
    ///   `declExpr`, not a Pratt-level operand, so feeding it to
    ///   `parse_pratt_expr` would reach the atomic const parser's `unreachable!`.
    ///   Callers that want to accept such a bound dispatch
    ///   [`Self::parse_open_lower_range`] directly (`.. ..3`, `1.. ..2`). The
    ///   ordinary `a..b..c` path still keeps non-leading uppers as Pratt operands
    ///   so the range loop remains left-associative. The bare `*` wildcard is
    ///   **not** excluded — it is a high-precedence atom
    ///   ([`Parser::parse_index_wildcard`]), so a wildcard bound (`* .. 3`,
    ///   `1 .. *`) parses through `parse_pratt_expr`.
    /// * a LexFilter-swallowed `)`/`}` ([`Self::at_swallowed_seq_closer`]) — for
    ///   an open-upper range inside parens/braces (`(a..) + b`, `{ a.. } x`) the
    ///   filtered peek after `..` is already the token *past* the closer;
    ///   without this gate that outer token is wrongly taken as the upper bound
    ///   and the closer drained as `ERROR`. Mirrors [`Self::at_range_op`].
    fn peek_starts_range_bound(&self) -> bool {
        self.peek_is_expr_start() && !self.peek_is_range_op() && !self.at_swallowed_seq_closer()
    }

    /// `true` when the next filtered token is the `..` range operator and is
    /// not stranded past a LexFilter-swallowed `)`/`}`. The swallowed-closer
    /// gate mirrors the one [`Parser::parse_pratt_expr`]'s `<-` check uses:
    /// inside `( a )` the closer is stripped from the filtered stream, so a
    /// `..` *after* the closer surfaces as the body's immediate successor;
    /// bailing here lets the body stop at the closer so `(a)..b` is
    /// `IndexRange(Paren a, b)`, not `Paren(IndexRange(a, b))`.
    fn at_range_op(&self) -> bool {
        self.peek_is_range_op() && !self.at_swallowed_seq_closer()
    }

    /// `true` when the next filtered token is the bare `..` range operator
    /// ([`Token::DotDot`]), ignoring the swallowed-closer nuance. Used by
    /// [`Self::at_range_op`] (the range-continuation `..`) and by the bound /
    /// prefix-operand exclusions ([`Self::peek_starts_range_bound`],
    /// [`Parser::parse_minus_expr`] / [`Parser::parse_address_of`] /
    /// [`Parser::parse_inferred_cast`]): an open-lower range `..e` is a
    /// `declExpr`, *not* a `minusExpr`, so it cannot be the operand of a unary
    /// `-`/`&`/`upcast` (FCS rejects `- ..3` / `& ..3`) — those sites exclude it
    /// so the operand recursion never feeds a leading `..` into the atomic-level
    /// const parser (which would `unreachable!`). The bare `*` wildcard is
    /// **not** lumped in here: it is a `declExpr` leaf
    /// ([`Parser::parse_index_wildcard`]) the infix / range loops handle directly.
    pub(super) fn peek_is_range_op(&self) -> bool {
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::DotDot)), _))
        )
    }

    /// `true` when the next filtered token is the lone whole-dimension wildcard
    /// `*` ([`Token::Op`]`("*")`). At a fresh range / Pratt head this is FCS's
    /// nullary `STAR` leaf ([`Parser::parse_index_wildcard`]); the
    /// [`Parser::parse_minus_expr`] wildcard arm and the `<-` gate in
    /// [`Parser::parse_pratt_expr`] consult it. (Elsewhere `*` is infix
    /// multiplication, consumed by the Pratt operator loop, which never reaches
    /// this — by then the `*` is no longer the head token.)
    pub(super) fn peek_is_index_wildcard_star(&self) -> bool {
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Op("*"))), _))
        )
    }

    /// Pratt-style infix-operator climber. Parses one
    /// [`Parser::parse_app_expr`] atom and then, while the next filtered
    /// token classifies as an infix operator with left-binding-power
    /// `lbp >= min_bp`, wraps the already-emitted LHS into FCS's
    /// `mkSynInfix` two-tier shape and recurses for the RHS at the
    /// operator's right-binding-power. The shape mirrors FCS exactly:
    ///
    /// ```text
    /// APP_EXPR (outer, isInfix=false, funcExpr=inner, argExpr=rhs)
    /// ├── INFIX_APP_EXPR (inner, isInfix=true, funcExpr=op, argExpr=lhs)
    /// │   ├── <lhs>
    /// │   └── LONG_IDENT_EXPR > LONG_IDENT > IDENT_TOK("+")
    /// └── <rhs>
    /// ```
    ///
    /// Source-order children inside `INFIX_APP_EXPR` are `[lhs, op]`
    /// (which is `a +` in source), not the FCS argument order `[op, lhs]`.
    /// The typed-AST accessors [`crate::syntax::AppExpr::func`] /
    /// [`crate::syntax::AppExpr::arg`] swap them back for FCS-faithful
    /// projection while keeping the lossless `text(tree) == source`
    /// invariant.
    ///
    /// Left-associative ops set `lbp = rbp + 1` so the recursive call
    /// stops on the next same-precedence op and the outer iteration
    /// picks it up against the already-built tree. Right-associative
    /// ops set `lbp = rbp` so the recursive call swallows further
    /// same-precedence ops into one right-leaning chain.
    pub(super) fn parse_pratt_expr(&mut self, min_bp: u16) {
        // Depth-guarded: this is the universal expression recursion chokepoint.
        // Every nested expression reaches it — `parse_expr` descends through the
        // tuple/range levels into `parse_pratt_expr(0)`; delimited and control
        // atoms (parens, `if`, `match`, computation expressions, range bounds)
        // re-enter it; and right-associative operators (`::`, infix RHS) recurse
        // via `parse_pratt_expr(rbp)` below. Bounding it bounds them all.
        self.with_depth(|p| {
            // The `if`-dispatch lives in `parse_minus_expr` so the recursive
            // operand path (e.g. `- if ...`, `& if ...`) also intercepts it,
            // rather than falling through to atomic-level `parse_const_expr`
            // and panicking. Either way the else-branch's inner
            // `parse_pratt_expr(0)` greedily absorbs trailing infix/tuple to
            // the right (`pars.fsy:4324 IF declExpr ifExprCases %prec expr_if`,
            // precedence pars.fsy:323), so the outer Pratt loop here finds no
            // continuation after an if-then-else.
            let cp = p.builder.checkpoint();
            // Whether the bare LHS this frame is about to build is the `*`
            // whole-dimension wildcard. FCS's `STAR` is a `declExpr` leaf, **not** a
            // `minusExpr`, so — like an infix App or a cast — it cannot be a `<-`
            // assignment LHS (`* <- 1` is an FCS error). Captured *before*
            // `parse_minus_expr` consumes the `*` (at a fresh Pratt head a leading
            // `Op("*")` always reduces to the wildcard arm), and folded into the
            // `<-` gate below alongside `built_continuation`.
            let lhs_is_wildcard = p.peek_is_index_wildcard_star();
            // Whether the bare LHS this frame is about to build is a `fixed e`
            // (`FIXED declExpr` → `SynExpr.Fixed`). Like the `*` wildcard, a
            // `FIXED_EXPR` is a `declExpr`, **not** a `minusExpr`, so it cannot be a
            // `<-` assignment LHS — FCS rejects `fixed … <- v`. Captured here (a
            // fresh Pratt head with a leading `Token::Fixed` always dispatches to
            // [`Parser::parse_fixed`], which parses the *full* `declExpr` operand and
            // can return with a `<-` still pending when that operand built a cast /
            // wildcard that declined it — `fixed a :> T <- v`, `fixed * <- v`). The
            // tighter `lazy`/`assert` never need this: their operand either folds the
            // `<-` in itself or leaves a `built_continuation` cast in *this* frame.
            let lhs_is_fixed = matches!(p.peek(), Some((Ok(FilteredToken::Raw(Token::Fixed)), _)));
            p.parse_minus_expr();
            p.continue_pratt_expr(cp, min_bp, lhs_is_wildcard, lhs_is_fixed);
        });
    }

    /// The infix / cons / type-relation / `<-` continuation of
    /// [`Self::parse_pratt_expr`], split out so the appExpr-headed brace handler
    /// ([`Self::parse_app_head_brace`]) can resume the climb from an
    /// already-parsed `appExpr` LHS at `cp` (an `appExpr` is a `minusExpr`, so it
    /// is a valid LHS for this loop; `lhs_is_wildcard` and `lhs_is_fixed` are
    /// `false` there).
    /// Behaviourally identical to the inline tail it replaces.
    fn continue_pratt_expr(
        &mut self,
        cp: rowan::Checkpoint,
        min_bp: u16,
        lhs_is_wildcard: bool,
        lhs_is_fixed: bool,
    ) {
        // Tracks whether this loop built *any* continuation (infix App or
        // type-relation cast) on top of the bare `parse_minus_expr` result. FCS's
        // `<-` assignment binds a `minusExpr` LHS (`pars.fsy:4661`); an infix App
        // and a `:?` / `:>` / `:?>` cast are both `declExpr`, not `minusExpr`, so
        // the `<-` block below fires only when the loop built nothing (the LHS is
        // a bare `minusExpr`). When the loop built something, the `<-` must not
        // bind it at this frame:
        //   * For an infix, a following `<-` was already consumed in the
        //     operator's recursive RHS frame against the genuine `minusExpr`
        //     there (`a + b <- c` → `a + (b <- c)`), so none is pending here.
        //   * If that recursive frame *declined* the `<-` because it built a cast
        //     (`a < b :? T <- c`: the inner frame refuses to assign to
        //     `b :? T`), the `<-` survives to this frame — and FCS rejects it
        //     too (neither the cast nor the comparison is a `minusExpr` LHS), so
        //     leaving it for enclosing recovery (→ error) is correct.
        //   * A bare cast (`a :> T <- v`) likewise leaves the `<-` for recovery.
        let mut built_continuation = false;
        // One unified continuation loop covering both the `mkSynInfix` operators
        // and the type-relation operators (`:?` / `:>` / `:?>`), so the two
        // interleave at the same checkpoint: `a :?> b < c` builds the cast, then
        // re-checks for `<` against the whole `Downcast` (→ `(a :?> b) < c`),
        // which a separate type-op loop after the infix loop could not do.
        loop {
            if let Some((lbp, rbp)) = self.peek_infix_continuation() {
                if lbp < min_bp {
                    break;
                }
                // Inner `App(NonAtomic, isInfix=true, op, lhs)`. Children in
                // source order: lhs (already emitted between cp and now) +
                // the op wrapped as a single-segment SynLongIdent.
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::INFIX_APP_EXPR));
                self.emit_infix_op_as_long_ident();
                self.builder.finish_node();

                // Outer `App(NonAtomic, isInfix=false, inner, rhs)`. Children
                // in source order: the just-closed INFIX_APP_EXPR + the
                // RHS parsed at the operator's right-binding-power.
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_EXPR));
                self.parse_pratt_expr(rbp);
                self.builder.finish_node();
                built_continuation = true;
                continue;
            }
            // `::` — the cons operator, `SynExpr` `declExpr COLON_COLON declExpr`
            // (`pars.fsy:4765`). A distinct [`SyntaxKind::CONS_EXPR`] node (not
            // `mkSynInfix`): FCS lowers it to a single `App(op_ColonColon,
            // Tuple([lhs; rhs]))`, projected by the normaliser. Right-associative
            // (`rbp == lbp`), so the recursive RHS swallows further `::`s into one
            // right-leaning chain (`a :: b :: c` ⇒ `a :: (b :: c)`).
            if let Some((lbp, rbp)) = self.peek_cons_continuation() {
                if lbp < min_bp {
                    break;
                }
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::CONS_EXPR));
                self.bump_into(SyntaxKind::COLON_COLON_TOK);
                // The RHS exists (gated by `peek_cons_continuation`'s
                // RHS-must-exist check), so recurse at the cons rbp.
                self.parse_pratt_expr(rbp);
                self.builder.finish_node();
                built_continuation = true;
                continue;
            }
            // `:?` / `:>` / `:?>` — `SynExpr.TypeTest` / `Upcast` / `Downcast`.
            // Distinct nodes (not `mkSynInfix`), each taking a *type* on the
            // right (`parse_type`, which self-recovers on a missing type to match
            // FCS's `… recover` arm). Left-associative re-wrap falls out of
            // looping at the same `cp`.
            if let Some((node_kind, op_tok, lbp)) = self.peek_type_op_continuation() {
                if lbp < min_bp {
                    break;
                }
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(node_kind));
                self.bump_into(op_tok);
                self.parse_type();
                self.builder.finish_node();
                built_continuation = true;
                continue;
            }
            // `in` — the query computation-expression join operator,
            // `SynExpr.JoinIn` (`declExpr JOIN_IN declExpr`, `pars.fsy:4669`).
            // LexFilter rewrites the `in` to `Virtual::JoinIn` only inside a
            // brace CE (`detect_join_in_ctxt`), so a dedicated
            // [`SyntaxKind::JOIN_IN_EXPR`] node is built here (not the
            // `mkSynInfix` two-tier App). Left-associative at the `||`/`or`
            // band (`rbp == lbp + 1`), so a second `in` re-wraps at `cp`
            // (`a in b in c` ⇒ `(a in b) in c`).
            if let Some((lbp, rbp)) = self.peek_join_in_continuation() {
                if lbp < min_bp {
                    break;
                }
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::JOIN_IN_EXPR));
                // The `in`'s end byte, captured before the operator is consumed,
                // for the swallowed-closer probe below.
                let in_end = self.peek().map(|(_, s)| s.end);
                self.emit_join_in_token();
                // The RHS is a full `declExpr` (FCS `declExpr JOIN_IN declExpr`).
                // A leading open-lower range (`query { a in ..b }`, FCS's
                // `DOT_DOT declExpr`) is a `declExpr` form that `parse_pratt_expr`
                // *cannot* consume — a leading `..` falls through to the atomic
                // const parser's `unreachable!`. So a `..` RHS is delegated to the
                // shared open-lower-range production, which self-recovers on a
                // missing upper (`query { a in .. }`). A *bounded* `a in b..c`
                // needs no special case: `..` binds looser than `JOIN_IN`
                // (`IndexRange(JoinIn(a, b), c)`), so the `b` RHS parses normally
                // and the enclosing range level takes the `..c`.
                //
                // Otherwise recurse at the join rbp, with two recovery gates that
                // leave the RHS for enclosing recovery instead:
                //
                // * **Swallowed-closer gate** (same as the cons / infix
                //   continuations): the enclosing brace/paren's `}` / `)` is
                //   stripped from the *filtered* stream, so on an incomplete join
                //   (`query { a in } b`) `peek_is_expr_start` would peer past the
                //   `}` and wrongly take the outer `b` as the RHS, draining the
                //   real `}` as an error *inside* the `JOIN_IN_EXPR`. If the next
                //   non-trivia *raw* token after the `in` is that swallowed closer,
                //   the RHS is missing — stop so the brace closes with `b` outside.
                // * **RHS-must-exist** (`query { x in }`): the operand position is
                //   empty / non-startable.
                //
                // Either way a missing-operand error keeps the node lossless,
                // mirroring FCS's `… JOIN_IN ends_coming_soon_or_recover`
                // recovery (`pars.fsy:4672`).
                let rhs_after_swallowed_closer = in_end.is_some_and(|e| {
                    matches!(
                        self.next_non_trivia_raw_after(e),
                        Some(Token::RParen | Token::RBrace)
                    )
                });
                if self.at_range_op() {
                    self.parse_open_lower_range();
                } else if !rhs_after_swallowed_closer && self.peek_is_expr_start() {
                    self.parse_pratt_expr(rbp);
                } else {
                    self.push_missing_operand_error();
                }
                self.builder.finish_node();
                built_continuation = true;
                continue;
            }
            break;
        }

        // `<-` assignment (`pars.fsy:4661 minusExpr LARROW declExprBlock`,
        // dispatched by `mkSynAssign`). The lowest-precedence,
        // right-associative operator (`pars.fsy:343 %right LARROW`), but its
        // LHS is grammatically a `minusExpr` — exactly the expression parsed
        // into `cp..now` at *this* recursion frame. We check it
        // unconditionally (not gated by `min_bp`): the `<-` always binds the
        // immediately-preceding minus/app expression, which is what makes
        // `a + b <- c` parse as `a + (b <- c)` — the inner Pratt frame
        // handling `+`'s RHS reaches this check with just `b` built, so the
        // assignment wraps `b`, not `a + b`. The RHS is a full
        // `declExprBlock` (tuple- and offside-block-inclusive), so
        // right-associativity (`x <- y <- z` ⇒ `x <- (y <- z)`) falls out of
        // the recursive RHS parse.
        //
        // `minusExpr`-LHS gate (`!built_continuation && !lhs_is_wildcard`): FCS's
        // `<-` binds a `minusExpr` (`pars.fsy:4661`), so this block fires only
        // when the LHS is a bare `minusExpr` — the loop built no continuation
        // *and* the bare result was not the `*` wildcard. If the loop built an
        // infix App or a `:?` / `:>` / `:?>` cast (both `declExpr`, not
        // `minusExpr`), the `<-` must not bind it here: for an infix the `<-` was
        // already consumed in the operator's recursive RHS frame (`a + b <- c` →
        // `a + (b <- c)`); for a cast — or an infix whose RHS frame *declined*
        // the `<-` because it built a cast (`a < b :? T <- c`) — FCS rejects the
        // `<-` (no valid `minusExpr` LHS), so leaving it for enclosing recovery
        // (which records the "unexpected token after binding expression" error
        // and stays lossless) matches FCS. The `*` wildcard is the same case: a
        // `declExpr` leaf, so `* <- 1` leaves the `<-` for recovery (FCS error).
        // A `fixed e` is likewise a `declExpr` (`lhs_is_fixed`): its operand may
        // decline a trailing `<-` (`fixed a :> T <- v`, `fixed * <- v`), and FCS
        // rejects binding it, so leave the `<-` for recovery here too.
        //
        // Swallowed-closer gate (the same one `peek_infix_continuation`
        // applies): inside `( x )` / `{ x }` the closer is stripped from the
        // filtered stream, so a `<-` *after* the closer surfaces as the
        // immediate filtered successor of the body. Without this guard
        // `(x) <- 1` would wrongly build the assignment *inside* the paren
        // (`Paren(Set(x, 1))`); bail so the body stops at the closer and the
        // assignment binds the whole `Paren` from the enclosing frame.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LArrow)), _))
        ) && !built_continuation
            && !lhs_is_wildcard
            && !lhs_is_fixed
            && !self.at_swallowed_seq_closer()
        {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::ASSIGN_EXPR));
            self.bump_into(SyntaxKind::LARROW_TOK);
            self.parse_assign_rhs();
            self.builder.finish_node();
        }
    }

    /// `true` if the current filtered token can lead a fresh expression
    /// — used at every "top of an expression" context (file top, module
    /// decl body, infix RHS via [`Parser::is_expr_start_at`], paren-expr
    /// body, tuple element start). Defers to [`raw_starts_minus_expr`]:
    /// atomic starters plus minus-level prefixes (`-`, `+`, `&`, `&&`,
    /// `%`, `%%`, `+.`, `-.`).
    ///
    /// Arg-position uses a *narrower* check ([`Parser::peek_starts_app_arg`]):
    /// only atomic + ADJACENT_PREFIX_OP, since `f - x` is infix
    /// application, not `f (-x)`.
    pub(super) fn peek_is_expr_start(&self) -> bool {
        self.expr_start_at(self.pos)
    }

    /// [`Self::peek_is_expr_start`] at an arbitrary offset into the filtered
    /// stream rather than always at the cursor. This is the *full*
    /// `declExpr`-level starter set — `do`/`do!`/`let!`/leading `..`/`function`/
    /// `fun`/… — i.e. exactly what [`Self::parse_seq_block_body`] would accept as
    /// a leading expression. It is deliberately *wider* than the infix-RHS
    /// [`Self::is_expr_start_at`] (which excludes `declExpr`-only leaves like a
    /// leading `..` or `do`). Used by the module let-in dispatch
    /// ([`Self::parse_module_let`]) to decide whether a body follows the `in`.
    pub(super) fn expr_start_at(&self, offset: usize) -> bool {
        match self.filtered_tokens.get(offset) {
            // `LParen` is in `raw_starts_minus_expr`, so this arm must
            // come first — the wildcard one would otherwise short-circuit
            // and skip the lex-error lookahead that gates paren-expr. The paren
            // body is a full expression (incl. unit `()` and a block `let`/
            // `use`) — see [`raw_after_lparen_starts_expr`], the shared
            // predicate every `(`-after lookahead uses.
            Some((Ok(FilteredToken::Raw(Token::LParen)), lparen_span)) => self
                .next_non_trivia_raw_after(lparen_span.end)
                .is_some_and(raw_after_lparen_starts_expr),
            Some((Ok(FilteredToken::Raw(t)), _)) if raw_starts_minus_expr(t) => true,
            // `_.member` — the accessor-function shorthand (FCS's `UNDERSCORE
            // DOT atomicExpr`). `Underscore` is deliberately *not* in
            // `raw_starts_atomic_expr` (a bare `_` is not an expression), so the
            // two-token `_.` shape is recognised here via the cursor-relative
            // lookahead. Lets `_.Foo` head a binding RHS, a tuple element, etc.
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _)) if self.at_dot_lambda(offset) => {
                true
            }
            // `?ident` — the optional-named-argument expression (FCS's `QMARK
            // nameop`). Like `_.`, `?` is *not* in `raw_starts_atomic_expr` (a
            // bare `?` is the dynamic operator, postfix), so the two-token shape
            // is recognised here via the cursor-relative lookahead — admitting
            // `?opt` as a binding RHS, a tuple element, etc.
            Some((Ok(FilteredToken::Raw(Token::QMark)), _))
                if self.qmark_opens_optional_arg_at(offset) =>
            {
                true
            }
            // `..upper` — a leading `..` range (FCS's `DOT_DOT declExpr`
            // production). This is a `declExpr`-level starter only (not an
            // atomic / arg starter), so it lives here rather than in
            // `raw_starts_minus_expr`: [`Parser::parse_expr`] →
            // [`Parser::parse_range_expr`] handles the open-lower form, and
            // the indexer tail / seq-block body gates admit `arr.[..3]`.
            Some((Ok(FilteredToken::Raw(Token::DotDot)), _)) => true,
            // `*` — the whole-dimension wildcard (FCS's nullary `STAR` declExpr,
            // `IndexRange(None, None)`). A `declExpr`-level starter, like a
            // leading `..`: it admits `arr.[*]` / `[*]` / `(1, *)` through the
            // indexer / seq-block / tuple gates, and [`Parser::parse_range_expr`]
            // emits the wildcard node. *Not* in `raw_starts_atomic_expr` /
            // `peek_starts_app_arg`, so `f * x` stays infix multiplication (the
            // wildcard only fires at a fresh range-expression head, never after
            // an operand). FCS rejects a bare top-level `let r = *` on a layout
            // offside rule we don't replicate — we accept it as a lossless
            // `IndexRange(None, None)` (a lenient, no-wrong-tree divergence; see
            // the parser-plan Known leniencies).
            Some((Ok(FilteredToken::Raw(Token::Op("*"))), _)) => true,
            // `^expr` — the from-end index/slice prefix (FCS's `minusExpr:
            // INFIX_AT_HAT_OP minusExpr`, `SynExpr.IndexFromEnd`). A
            // `minusExpr`-level prefix valid wherever a `declExpr` is (`let i =
            // ^1`, `[ ^1 ]`, `arr.[^1]`), dispatched by [`Parser::parse_minus_expr`].
            // Only a *leading* `^` is from-end; a mid-expression `a ^ b` is the
            // infix `^` the Pratt loop owns, never reaching this head gate. Kept
            // out of `peek_starts_app_arg`, so `f ^1` stays infix.
            Some((Ok(FilteredToken::Raw(Token::Op("^"))), _)) => true,
            // `Virtual::Fun` is LexFilter's rewrite of `Token::Fun`. The
            // raw `Token::Fun` is in `raw_starts_minus_expr`, but the
            // filtered stream only ever surfaces the virtual at this
            // position (LexFilter's `pushCtxt CtxtFun`), so the raw arm
            // above would never fire for `fun`. Recognise it here so
            // `fun … -> …` is an expression starter at file top, in
            // tuple elements, in paren bodies, etc.
            Some((Ok(FilteredToken::Virtual(Virtual::Fun)), _)) => true,
            // `Virtual::DoBang` — LexFilter's rewrite of `Token::DoBang`.
            // Like `fun`, the filtered stream only surfaces the virtual, so
            // recognise it here (`do! e` is an expression-start in a CE body,
            // a paren body, etc.). Dispatched in `parse_minus_expr`.
            Some((Ok(FilteredToken::Virtual(Virtual::DoBang)), _)) => true,
            // `Virtual::Do` — LexFilter's rewrite of a `do` in statement
            // position (`hardwhiteDoBinding` → `SynExpr.Do`). Like `do!`, only
            // the virtual surfaces, so recognise `do e` as an expression starter
            // (a module-level decl via `EXPR_DECL`, a sequence/CE-body element,
            // a paren body). Dispatched in `parse_minus_expr`. The `do` of a
            // `while`/`for` loop never reaches this predicate: that condition is
            // parsed by `parse_expr`, which terminates *before* the `do` (the
            // `do` is no tuple/range/infix/app-arg continuation), and the loop's
            // own `parse_do_block_body` then claims the `Virtual::Do`.
            Some((Ok(FilteredToken::Virtual(Virtual::Do)), _)) => true,
            // `Virtual::Binder` — LexFilter's rewrite of `Token::LetBang`/
            // `UseBang` in a block-let `CtxtLetDecl`. Like `do!`, only the
            // virtual surfaces, so recognise `let!`/`use!` as an expression
            // starter (CE body, etc.). The non-block raw-`BINDER` form is not
            // an expr starter here (it isn't parsed yet — see
            // `parse_minus_expr`). `Virtual::AndBang` is deliberately *not*
            // here: an `and!` only continues an open binder group, never
            // starting a fresh expression.
            Some((Ok(FilteredToken::Virtual(Virtual::Binder)), _)) => true,
            // `Virtual::Let` — LexFilter's `OffsideLet` rewrite of a
            // `Token::Let`/`Use` in expression position (a function/`let`/
            // `fun`/`if`/`match` body, a paren body). Like the binder, only the
            // virtual surfaces, so recognise it here as an expression starter;
            // `parse_minus_expr` dispatches it to `parse_let_or_use_expr`. A
            // *module-level* `let` never reaches this predicate — the module
            // loop's `Virtual::Let` arm intercepts it first.
            Some((Ok(FilteredToken::Virtual(Virtual::Let)), _)) => true,
            // A *non-block* `let … in` — a raw `Token::Let` in the filtered stream
            // (the block-leading form is the `Virtual::Let` arm above).
            // Mid-expression operands (a tuple element `1, let …`, a
            // `lazy`/`assert`/`fixed` operand) surface this way, and
            // `parse_minus_expr` dispatches it to `parse_let_or_use_expr`. Kept out
            // of `raw_starts_minus_expr` on purpose (that classifier is also read
            // on the raw stream in decl context, where a module-level `let` is a
            // raw `Token::Let`), so it is admitted here instead. (`use` is excluded
            // — FCS relabels a non-block `use … in` to `Let`; see the
            // `parse_minus_expr` dispatch note.)
            Some((Ok(FilteredToken::Raw(Token::Let)), _)) => true,
            // `Virtual::Function` (`OFUNCTION`) — LexFilter's rewrite of
            // `Token::Function`. Recognised here for the same reason as
            // `Virtual::Fun`: the filtered stream only ever surfaces the
            // virtual, so `function … -> …` must be classified as an
            // expression starter at file top, on a `let` RHS, in tuple
            // elements, in paren bodies, etc.
            Some((Ok(FilteredToken::Virtual(Virtual::Function)), _)) => true,
            // `Virtual::Lazy`/`Virtual::Assert` (`OLAZY`/`OASSERT`) — LexFilter's
            // rewrite of an offside/control-flow `lazy`/`assert`. Only the virtual
            // surfaces for that form, so recognise it as an expression starter
            // (a `let` RHS, a paren body, a seq-block element); `parse_minus_expr`
            // dispatches it to `parse_lazy_or_assert`. The same-line form is a raw
            // `Token::Lazy`/`Assert`, already admitted via `raw_starts_minus_expr`.
            Some((Ok(FilteredToken::Virtual(Virtual::Lazy | Virtual::Assert)), _)) => true,
            _ => false,
        }
    }

    /// `true` if the current filtered token can lead an application
    /// argument. Mirrors `pars.fsy:5197 argExpr`: atomic-level starters
    /// (via [`raw_starts_atomic_expr`]) plus the ADJACENT_PREFIX_OP
    /// rewrite (via [`Parser::op_is_adjacent_prefix`]). Crucially this
    /// is *narrower* than [`Parser::peek_is_expr_start`] — a bare
    /// minus-level prefix without the adjacency-fire condition (e.g.
    /// `f - x`) is NOT an arg, because FCS's grammar promotes those to
    /// infix at the outer Pratt layer. The LParen-body lookahead still
    /// peers for a minus-level starter (paren-expr body is a full
    /// expression), so `f (-x)` parses correctly as `App(f, Paren(-x))`.
    pub(super) fn peek_starts_app_arg(&self) -> bool {
        match self.peek() {
            // A paren *argument* (`f (let x = 1 in x)`, `f (-x)`, `f ()`) — the
            // body is a full expression, so use the shared `(`-after predicate
            // ([`raw_after_lparen_starts_expr`]), which also admits a block
            // `let`/`use` after `(`.
            Some((Ok(FilteredToken::Raw(Token::LParen)), lparen_span)) => self
                .next_non_trivia_raw_after(lparen_span.end)
                .is_some_and(raw_after_lparen_starts_expr),
            Some((Ok(FilteredToken::Raw(t)), _)) if raw_starts_atomic_expr(t) => true,
            // `_.member` — the accessor-function shorthand as a bare application
            // argument (`List.map _.Length xs`). `Underscore` is not an atomic
            // starter on its own, so the `_.` shape is recognised here.
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _))
                if self.at_dot_lambda(self.pos) =>
            {
                true
            }
            // `?ident` — the optional-named-argument expression as a bare
            // application argument (`f ?opt`). An `atomicExpr` like `_.`, so the
            // same two-token shape applies.
            Some((Ok(FilteredToken::Raw(Token::QMark)), _)) if self.qmark_opens_optional_arg() => {
                true
            }
            Some(_) => self.op_is_adjacent_prefix(),
            None => false,
        }
    }

    /// `true` if the current filtered token can lead an `atomicExprAfterType`
    /// argument — the constructor / attribute argument grammar shared by
    /// `new T(…)` ([`Parser::parse_new_expr`]), `inherit T(…)`
    /// ([`Parser::parse_inherit_member`]), and `[<Attr(…)>]`
    /// ([`Parser::parse_attribute`]); FCS's `opt_atomicExprAfterType`
    /// (`pars.fsy:5655`).
    ///
    /// Narrower than [`Self::peek_is_expr_start`]: `atomicExprAfterType`
    /// **excludes** the `identExpr: opName` alternative, so a parenthesised
    /// operator-value is not a valid argument here — `new C(+)` / `[<A(=)>]` /
    /// `new C(*)` are FCS errors even though `(+)` is a fine expression
    /// elsewhere. The `( op )` form is rejected by the extra
    /// [`Self::at_paren_op_value`] guard on the `(` arm; the glued `(*)` and a
    /// bare ident / prefix-op are rejected by [`raw_starts_attribute_arg`]. A
    /// genuinely paren-*wrapped* operator-value (`new C((+))`) is still
    /// admitted: its head token is the outer `(`, and `at_paren_op_value` only
    /// fires on the operator-immediately-`)` shape.
    pub(super) fn peek_starts_aftertype_arg(&self) -> bool {
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::LParen)), _)) => {
                // An active-pattern name `(|Foo|_|)` is an `identExpr: opName`,
                // the same `opName` alternative `atomicExprAfterType` excludes —
                // so `new C(|Foo|_|)` / `[<A(|Foo|_|)>]` are FCS errors, just
                // like `new C(+)`. Reject it here alongside the operator-value.
                self.peek_is_expr_start()
                    && !self.at_paren_op_value(self.pos)
                    && !self.at_active_pat_name()
            }
            Some((Ok(FilteredToken::Raw(t)), _)) => raw_starts_attribute_arg(t),
            _ => false,
        }
    }

    /// `true` if the current filtered token can lead an *atomicExpr*
    /// (`pars.fsy:5211`). The LParen-body lookahead still peers for a
    /// minus-level starter inside the parens (a paren-expr body is a
    /// full expression), but the leading-token set is the narrower
    /// atomic level. Used by recursive prefix-form parsers to recover
    /// when the operand position lacks an atom (`!)`, `! \n`, etc.).
    pub(super) fn peek_starts_atomic_expr(&self) -> bool {
        match self.peek() {
            // Paren body is a full expression — shared `(`-after predicate
            // (see [`raw_after_lparen_starts_expr`], incl. a block `let`/`use`).
            Some((Ok(FilteredToken::Raw(Token::LParen)), lparen_span)) => self
                .next_non_trivia_raw_after(lparen_span.end)
                .is_some_and(raw_after_lparen_starts_expr),
            Some((Ok(FilteredToken::Raw(t)), _)) if raw_starts_atomic_expr(t) => true,
            // `_.member` — the accessor-function shorthand is an `atomicExpr`,
            // so prefix-form operand recovery accepts it (`! _.Foo`). Recognised
            // via the `_.` two-token shape, like the other gates.
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _))
                if self.at_dot_lambda(self.pos) =>
            {
                true
            }
            // `?ident` — the optional-named-argument expression is an
            // `atomicExpr`, so prefix-form operand recovery accepts it
            // (`! ?opt`). Same two-token shape as the `_.` arm.
            Some((Ok(FilteredToken::Raw(Token::QMark)), _)) if self.qmark_opens_optional_arg() => {
                true
            }
            _ => false,
        }
    }
}
