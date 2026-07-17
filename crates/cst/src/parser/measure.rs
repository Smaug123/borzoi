//! Unit-of-measure annotated numeric literals in expression position — FCS's
//! `rawConstant HIGH_PRECEDENCE_TYAPP measureTypeArg` (`pars.fsy:3521`),
//! projecting to `SynExpr.Const(SynConst.Measure(constant, range, synMeasure,
//! trivia))`.
//!
//! The `measureTypeExpr` grammar (`pars.fsy:6693-6760`) is a small closed
//! recursive grammar; this is its recursive-descent transcription. The exponent
//! of a `^` power reuses the `RATIONAL_CONST_*` machinery shared with the
//! type-side measure-power ([`super::Parser::parse_atomic_rational_const`]).
//!
//! Precedence (loosest → tightest): `*` / `/` (left-associative, one level) <
//! juxtaposition (`Seq`) < `^` (`Power`). Every `measureTypeExpr` is wrapped in
//! a `Seq` by FCS, so even a single named measure `<m>` is `Seq[Named ["m"]]`;
//! the sole exception is the anonymous `<_>`, reached through the dedicated
//! `measureTypeArg: LESS UNDERSCORE GREATER` arm and therefore *not* wrapped.

use super::*;

impl<'src> Parser<'src> {
    /// Wrap the `CONST_EXPR` checkpointed at `cp` and the trailing `< … >`
    /// measure annotation in a [`SyntaxKind::MEASURE_LIT_EXPR`]. The caller
    /// ([`Self::parse_postfix_tail`]) has verified the cursor sits at the
    /// `HighPrecedenceTyApp` adjacency virtual that precedes the `<` and that
    /// the head literal is numeric ([`Self::prev_filtered_is_measure_numeric`]).
    ///
    /// The `HighPrecedenceTyApp` virtual is consumed zero-width as an `ERROR`
    /// (mirroring [`Self::parse_type_app_tail`]). The matching `>` was already
    /// found by LexFilter's adjacency walk (it is what made the marker fire), so
    /// it is present as a `Greater(true)` and bumped as `GREATER_TOK`.
    pub(super) fn parse_measure_lit_tail(&mut self, cp: rowan::Checkpoint) {
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEASURE_LIT_EXPR));
        // HPA/HPTA virtual: consume as a zero-width ERROR placeholder.
        self.bump_into(SyntaxKind::ERROR);
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Less(_))), _))
        ) {
            self.bump_into(SyntaxKind::LESS_TOK);
        }
        self.parse_measure_type_arg();
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Greater(_))), _))
        ) {
            self.bump_into(SyntaxKind::GREATER_TOK);
        }
        self.builder.finish_node(); // MEASURE_LIT_EXPR
    }

    /// `measureTypeArg` body (the part *inside* `< >`, the `<`/`>` are the
    /// caller's). Either the anonymous measure `_` ([`SyntaxKind::MEASURE_ANON`],
    /// the `LESS UNDERSCORE GREATER` arm) or a full `measureTypeExpr`.
    fn parse_measure_type_arg(&mut self) {
        // `< _ >` → `SynMeasure.Anon`. Only the bare `_` immediately before `>`
        // is the anonymous measure; `_` anywhere else is not a `measureTypeAtom`
        // and falls through to the (erroring) expression path. The close `>` is
        // checked on the *filtered* stream: a `>`-led tail (`1.0<_>.ToString()`,
        // `1.0<_>= y`) fuses the `>` into the next raw token (`Op(">.")`,
        // `Op(">=")`), and only LexFilter's filtered stream splits the closing
        // `Greater` back out — exactly the stream `parse_measure_lit_tail` bumps
        // the close from. (A raw lookahead would miss the split `>` and bail.)
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _))
        ) && matches!(
            self.next_non_trivia_filtered_after_pos(),
            Some(FilteredToken::Raw(Token::Greater(_)))
        ) {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEASURE_ANON));
            self.bump_into(SyntaxKind::UNDERSCORE_TOK);
            self.builder.finish_node();
            return;
        }
        self.parse_measure_type_expr();
    }

    /// `measureTypeExpr` (`pars.fsy:6744`) — the `*` / `/` layer over
    /// `measureTypeSeq`, left-associative, plus the no-numerator reciprocal
    /// `/ measureTypeExpr` (`SynMeasure.Divide(None, _)`).
    ///
    /// Each operand (the first, and the right of every `*` / `/`) is parsed by
    /// [`Self::parse_measure_operand`], which itself admits a leading `/`
    /// reciprocal. That single hook gives both the left-associative chain
    /// (`m / s / s` ⇒ `Divide(Divide(Seq[m], Seq[s]), Seq[s])` — each operand a
    /// plain `Seq`) *and* a reciprocal right-hand side (`m * /s` ⇒
    /// `Product(Seq[m], Divide(None, Seq[s]))`; FCS's binary RHS is a full
    /// `measureTypeExpr`, so it may lead with the prefix `/`).
    fn parse_measure_type_expr(&mut self) {
        let cp = self.builder.checkpoint();
        self.parse_measure_operand();
        // Left-associative `*` / `/` run.
        loop {
            match self.next_non_trivia_raw_at_pos() {
                Some(Token::Op("*")) => {
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEASURE_PRODUCT));
                    self.bump_into(SyntaxKind::STAR_TOK);
                    self.parse_measure_operand();
                    self.builder.finish_node();
                }
                Some(Token::Op("/")) => {
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEASURE_DIVIDE));
                    self.bump_into(SyntaxKind::SLASH_TOK);
                    self.parse_measure_operand();
                    self.builder.finish_node();
                }
                _ => break,
            }
        }
    }

    /// One operand of the `*` / `/` chain (and the whole-expression head): a
    /// `measureTypeSeq`, or a leading-`/` reciprocal `SynMeasure.Divide(None,
    /// _)`. The reciprocal node is opened with no left measure child, so the
    /// numerator-before-`SLASH_TOK` accessor reads it as the `None` numerator.
    ///
    /// The reciprocal *body* recurses through this same hook (FCS's
    /// `INFIX_STAR_DIV_MOD_OP measureTypeExpr` makes it a full `measureTypeExpr`),
    /// so a denominator that itself leads with `/` nests as another reciprocal
    /// (`/ /s` ⇒ `Divide(None, Divide(None, Seq[s]))`). The recursion is bounded
    /// to a leading-`/` run: a non-`/` body is a plain `Seq`, and any trailing
    /// `*`/`/` binds at the enclosing [`Self::parse_measure_type_expr`] loop, so
    /// `/s * m` stays `Product(Divide(None, Seq[s]), Seq[m])`.
    fn parse_measure_operand(&mut self) {
        if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Op("/"))) {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEASURE_DIVIDE));
            self.bump_into(SyntaxKind::SLASH_TOK);
            // Depth-guarded: a leading-`/` run (`< / / / … s >`) nests reciprocals
            // here, a recursion below the `parse_type` guard, so count each level.
            self.with_depth(|p| p.parse_measure_operand());
            self.builder.finish_node();
        } else {
            self.parse_measure_type_seq();
        }
    }

    /// `measureTypeSeq` (`pars.fsy:6737`) — one-or-more juxtaposed
    /// `measureTypePower`, always wrapped in a [`SyntaxKind::MEASURE_SEQ`]
    /// (FCS's `measureTypeExpr: measureTypeSeq { Seq … }`).
    fn parse_measure_type_seq(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEASURE_SEQ));
        let mut any = false;
        while self.at_measure_power_start() {
            self.parse_measure_type_power();
            any = true;
        }
        if !any {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected unit of measure".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // MEASURE_SEQ
    }

    /// `measureTypePower` (`pars.fsy:6717`) — a `measureTypeAtom` optionally
    /// raised to a rational power (`atom ^ rational`,
    /// [`SyntaxKind::MEASURE_POWER`]), or the bare integer `1`
    /// ([`SyntaxKind::MEASURE_ONE`]; FCS reports an error for any other integer
    /// but still produces `One`).
    fn parse_measure_type_power(&mut self) {
        // `INT32` → `SynMeasure.One`. FCS admits only `1` here; a different
        // integer is an error but still parses as `One`.
        if self.at_measure_one() {
            if !self.measure_int_is_one() {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "unexpected integer literal in unit of measure (only `1` is allowed)"
                        .to_string(),
                    span,
                });
            }
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEASURE_ONE));
            self.bump_into(SyntaxKind::INT32_LIT);
            self.builder.finish_node();
            return;
        }
        let cp = self.builder.checkpoint();
        self.parse_measure_type_atom();
        // `^` / `^-` power tail, reusing the rational-const exponent parser.
        if matches!(
            self.next_non_trivia_raw_at_pos(),
            Some(Token::Op("^" | "^-"))
        ) {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEASURE_POWER));
            self.bump_into(SyntaxKind::MEASURE_POWER_OP_TOK);
            self.parse_atomic_rational_const();
            self.builder.finish_node();
        }
    }

    /// `measureTypeAtom` (`pars.fsy:6706`) — a named measure (`path`,
    /// [`SyntaxKind::MEASURE_NAMED`]), a measure variable (`typar`,
    /// [`SyntaxKind::MEASURE_VAR`]), or a parenthesised measure
    /// ([`SyntaxKind::MEASURE_PAREN`]).
    fn parse_measure_type_atom(&mut self) {
        match self.next_non_trivia_raw_at_pos() {
            Some(Token::Ident(_) | Token::QuotedIdent(_) | Token::Global) => {
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEASURE_NAMED));
                self.parse_long_ident_path("unit of measure");
                self.builder.finish_node();
            }
            // A measure variable `typar` — the quote form `'u`
            // (`TyparStaticReq.None`) or the head-type form `^u`
            // (`TyparStaticReq.HeadType`, FCS's `INFIX_AT_HAT_OP ident`). The
            // sigil token kind (`QUOTE_TOK` vs `HAT_TOK`) carries the static-req
            // discriminant, which the normaliser reads back.
            Some(Token::Quote | Token::Op("^")) => {
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEASURE_VAR));
                let sigil =
                    if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Quote)), _))) {
                        SyntaxKind::QUOTE_TOK
                    } else {
                        SyntaxKind::HAT_TOK
                    };
                self.bump_into(sigil);
                if matches!(
                    self.peek(),
                    Some((
                        Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                        _
                    ))
                ) {
                    self.bump_into(SyntaxKind::IDENT_TOK);
                } else {
                    // FCS's `typar` production requires an identifier after the
                    // `'`/`^` sigil; a bare sigil (`1.0<'>`) is a parse error
                    // there. Emit the diagnostic so we fail loud on the
                    // malformed measure rather than silently accepting it — the
                    // sigil token is still consumed above, keeping the parse
                    // lossless.
                    let span = self
                        .peek()
                        .map(|(_, s)| s.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message: "expected measure-variable name after `'`/`^`".to_string(),
                        span,
                    });
                }
                self.builder.finish_node();
            }
            Some(Token::LParen) => {
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEASURE_PAREN));
                self.bump_into(SyntaxKind::LPAREN_TOK);
                // Depth-guarded: nested parenthesised measures (`1.0<((((m))))>`)
                // recurse here, a cycle below the `parse_type` guard.
                self.with_depth(|p| p.parse_measure_type_expr());
                self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
                self.builder.finish_node();
            }
            _ => {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected unit of measure".to_string(),
                    span,
                });
            }
        }
    }

    /// `true` when the cursor starts a `measureTypePower` — a named/typar/paren
    /// `measureTypeAtom` or the bare integer `1` (`measureTypePower: INT32`).
    /// The head-type typar `^u` (`Op("^")` + ident) is admitted here too; a
    /// bare `^` not followed by an ident is the power operator, not an atom.
    fn at_measure_power_start(&self) -> bool {
        match self.next_non_trivia_raw_at_pos() {
            Some(
                Token::Ident(_)
                | Token::QuotedIdent(_)
                | Token::Global
                | Token::Quote
                | Token::LParen,
            ) => true,
            Some(Token::Op("^")) => matches!(
                self.nth_significant_raw_at_pos(1),
                Some(Token::Ident(_) | Token::QuotedIdent(_))
            ),
            _ => self.at_measure_one(),
        }
    }

    /// `true` when the cursor is at an integer literal admissible as the
    /// dimensionless `1` measure (`measureTypePower: INT32`). Reuses the
    /// type-side exponent classifier — the same `INT32`-terminal set.
    fn at_measure_one(&self) -> bool {
        self.next_non_trivia_raw_at_pos()
            .is_some_and(token_is_int32_exponent)
    }

    /// `true` when the integer-literal at the cursor denotes `1` (the only value
    /// FCS admits at the `measureTypePower: INT32` arm without an error). Decodes
    /// the value in its radix ([`int32_exponent_is_one`]) so every spelling of
    /// `1` — `1`, `0x1`, `0o1`, `1l` — is the clean `One`.
    fn measure_int_is_one(&self) -> bool {
        self.next_non_trivia_raw_at_pos()
            .is_some_and(int32_exponent_is_one)
    }
}
