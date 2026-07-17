//! Application expression productions: function application / juxtaposition,
//! argument parsing, and the adjacency rules that decide where one
//! application continues versus a new declaration begins.

use super::*;

impl<'src> Parser<'src> {
    /// *Whitespace* function application `f x` — the level above the atomic
    /// (`atomicExpr`) layer and below tuples. The head and every argument is a
    /// full [`Parser::parse_atomic_expr`] (atom + its postfix tail: adjacent
    /// applications and dot/index access), so the *adjacent* application binds
    /// tighter than the whitespace one: `f g(x)` is `App(f, App(g, (x)))`, and
    /// `f x.Bar` is `App(f, x.Bar)` (FCS's `atomicExpr` is left-recursive and
    /// tighter than `appExpr argExpr` — `pars.fsy:5192`/`5247`). The
    /// high-precedence (adjacent) application `f(x)` is therefore handled
    /// entirely in the atomic tail ([`Parser::parse_postfix_tail`]); this loop
    /// only steps across *whitespace*-separated arguments. Reusing one
    /// `Checkpoint` keeps it left-associative: `f x y` is `App(App(f, x), y)`.
    pub(super) fn parse_app_expr(&mut self) {
        let cp = self.builder.checkpoint();
        self.parse_atomic_expr();
        self.check_adjacent_malformed_numeric();
        while self.at_app_continuation() {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_EXPR));
            self.parse_arg_expr();
            self.check_adjacent_malformed_numeric();
            self.builder.finish_node();
        }
    }

    /// `true` when the next two filtered tokens are a
    /// `Virtual::HighPrecedenceParenApp` virtual followed by a *well-formed*
    /// `(` — the atomic high-precedence application `f(x)`
    /// (`pars.fsy:5247 atomicExpr: atomicExpr HIGH_PRECEDENCE_PAREN_APP
    /// atomicExpr`), as distinct from a whitespace application `f (x)`. The
    /// paren body must itself start an expression or be a unit `()`; otherwise
    /// (`f(`, `f(+)`) the marker is not a continuation and falls through to the
    /// caller's recovery so `parse_atomic_expr`'s LParen dispatch never sees an
    /// input it would `unreachable!` on. Shared by [`Self::at_app_continuation`]
    /// (the app-expression loop) and [`Self::parse_enum_case_value`] (the enum
    /// value's `atomicExpr`).
    pub(super) fn peek_high_precedence_paren_app(&self) -> bool {
        if !self.peek_is_paren_app_marker() {
            return false;
        }
        let Some((Ok(FilteredToken::Raw(Token::LParen)), lparen_span)) =
            self.filtered_tokens.get(self.pos + 1)
        else {
            return false;
        };
        self.next_non_trivia_raw_after(lparen_span.end)
            .is_some_and(raw_after_lparen_starts_expr)
    }

    /// `true` when the next filtered token is a
    /// `Virtual::HighPrecedenceParenApp` — the atomic-application marker
    /// LexFilter inserts between an IDENT and an adjacent `(`.
    pub(super) fn peek_is_paren_app_marker(&self) -> bool {
        matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceParenApp)),
                _
            )),
        )
    }

    /// `true` when the next two filtered tokens are a
    /// `Virtual::HighPrecedenceBrackApp` virtual followed by `[` — the atomic
    /// high-precedence bracket indexer `arr[i]`
    /// (`pars.fsy:5242 atomicExpr HIGH_PRECEDENCE_BRACK_APP atomicExpr`), as
    /// distinct from a whitespace application of a list literal `arr [i]`. The
    /// LexFilter only emits the virtual immediately ahead of an adjacent `[`
    /// (never `[|`), so the `[` check is a defensive lock-step assertion rather
    /// than a real disambiguation. Handled in [`Parser::parse_postfix_tail`],
    /// which parses the `[…]` as an [`SyntaxKind::ARRAY_OR_LIST_EXPR`] argument.
    pub(super) fn peek_high_precedence_brack_app(&self) -> bool {
        matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceBrackApp)),
                _
            )),
        ) && matches!(
            self.filtered_tokens.get(self.pos + 1),
            Some((Ok(FilteredToken::Raw(Token::LBrack)), _)),
        )
    }

    /// `pars.fsy:5197 argExpr` — an application argument. Two forms:
    /// the plain `atomicExpr` (delegates to [`Parser::parse_atomic_expr`]),
    /// and `ADJACENT_PREFIX_OP atomicExpr` — a unary prefix operator
    /// that, per FCS's LexFilter rewrite (`LexFilter.fs:2694`), is
    /// glued to its operand with no whitespace AND has whitespace
    /// before it (`f -x`, `f &x`, `f &&y`). The latter form lowers to:
    /// * For `&` / `&&` ops → [`SyntaxKind::ADDRESS_OF_EXPR`]
    ///   (FCS's `mkSynPrefix` special-cases `~&`/`~&&` to `AddressOf`,
    ///   `SyntaxTreeOps.fs:485`).
    /// * For other ops (`-`, `+`, `+.`, `-.`, `%`, `%%`) →
    ///   `APP_EXPR > [op-as-long-ident, atomic operand]`, matching
    ///   `mkSynPrefix`'s `App` fallback.
    ///
    /// Crucially the operand is parsed at *atomic* level (not
    /// `minusExpr` like the minus-level form): `pars.fsy:5197` says
    /// `ADJACENT_PREFIX_OP atomicExpr`, so `f - -x` does not nest the
    /// inner `-x` under the outer `-` here — it stops at the outer
    /// adjacent rewrite, leaves the inner `-` for the next iteration's
    /// failure / outer-loop handling. The adjacency / left-gap gates
    /// live in [`Parser::op_is_adjacent_prefix`].
    pub(super) fn parse_arg_expr(&mut self) {
        if !self.op_is_adjacent_prefix() {
            self.parse_atomic_expr();
            return;
        }
        let (res, _) = self
            .peek()
            .cloned()
            .expect("op_is_adjacent_prefix true implies a peeked filtered token");
        let tok = match res {
            Ok(FilteredToken::Raw(t)) => t,
            _ => unreachable!("op_is_adjacent_prefix only succeeds on a Raw token"),
        };
        match tok {
            Token::Amp => self.parse_address_of_atomic(SyntaxKind::AMP_TOK),
            Token::AmpAmp => self.parse_address_of_atomic(SyntaxKind::AMP_AMP_TOK),
            Token::Op(_) => {
                let cp = self.builder.checkpoint();
                self.emit_prefix_op_as_long_ident();
                if self.peek_starts_atomic_expr() {
                    self.parse_atomic_expr();
                } else {
                    self.push_missing_operand_error();
                }
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_EXPR));
                self.builder.finish_node();
            }
            _ => {
                unreachable!("op_is_adjacent_prefix limits its caller to Amp/AmpAmp/Op tokens")
            }
        }
    }

    /// The arg-position counterpart of [`Parser::parse_address_of`]:
    /// emits an [`SyntaxKind::ADDRESS_OF_EXPR`] whose operand is parsed
    /// at *atomic* level (`pars.fsy:5197 argExpr: ADJACENT_PREFIX_OP
    /// atomicExpr` + `mkSynPrefix` AddressOf carve-out). The two
    /// callers differ only in the operand level — minus-level callers
    /// can chain `& - 1` as `AddressOf(true, App(~-, 1))`, while
    /// arg-level callers stop at the atom (the next minus-level prefix
    /// belongs to a sibling app step, not this AddressOf).
    pub(super) fn parse_address_of_atomic(&mut self, op_kind: SyntaxKind) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ADDRESS_OF_EXPR));
        self.bump_into(op_kind);
        if self.peek_starts_atomic_expr() {
            self.parse_atomic_expr();
        } else {
            self.push_missing_operand_error();
        }
        self.builder.finish_node();
    }

    /// Mirror of FCS's malformed-numeric-literal rule
    /// (`lex.fsl:515` — `(int | xint | float) ident_char+` → error)
    /// at the parser layer: emit a ParseError if the LHS just parsed
    /// was a bare numeric literal AND the next non-trivia raw token
    /// starts with an `ident_char` (letter / digit / `_` / `'`) that
    /// sits immediately at the numeric's end. Called once per
    /// `parse_app_expr` after the head atom, *before* the application
    /// loop kicks in, so the error fires regardless of whether the
    /// next token is itself a valid expression-starter (which would
    /// short-circuit `at_app_continuation`). Without this, `1true`
    /// would parse as two consecutive valid module decls (`1` and
    /// `true`) with no diagnostic — silently accepting input FCS
    /// rejects.
    ///
    /// Restricting to `ident_char` matters: punctuation like `,` `)`
    /// `(` `;` `=` is *also* adjacent in tuples and parens, but those
    /// are well-formed F# — FCS's malformed-numeric rule explicitly
    /// requires an ident-character follower. `1(2)` and `1"x"` are
    /// valid applications under FCS and must not trigger this error.
    pub(super) fn check_adjacent_malformed_numeric(&mut self) {
        let next = self
            .raw_tokens
            .iter()
            .skip(self.raw_pos)
            .find_map(|(res, span)| match res {
                Ok(t) if raw_is_trivia(t) => None,
                Ok(_) | Err(_) => Some(span.clone()),
            });
        let Some(next_span) = next else { return };
        if !self.prev_raw_is_adjacent_numeric_atom(next_span.start) {
            return;
        }
        if !self.next_starts_with_ident_char(next_span.start) {
            return;
        }
        let prev_span = self.raw_tokens[..self.raw_pos]
            .iter()
            .rev()
            .find_map(|(res, span)| match res {
                Ok(t) if raw_is_trivia(t) => None,
                Ok(_) => Some(span.clone()),
                Err(_) => None,
            })
            .expect("prev_raw_is_adjacent_numeric_atom returned true so there is a prev");
        self.errors.push(ParseError {
            message: format!(
                "malformed numeric literal: digits at {}..{} are adjacent to identifier characters at {}..{} with no separating whitespace",
                prev_span.start, prev_span.end, next_span.start, next_span.end,
            ),
            span: prev_span.start..next_span.end,
        });
    }

    /// `true` if the character at `byte` is an F# `ident_char`
    /// (`lex.fsl:326` — `letter | connecting_char | combining_char |
    /// formatting_char | digit | '\''`). Approximated via Rust's
    /// Unicode-aware `is_alphanumeric()` (which covers `\p{L}∪\p{N}`,
    /// the union of letters and numbers — including Greek letters like
    /// `π` that FCS sees as `letter`) plus `_` and `'`. The connecting/
    /// combining/formatting categories aren't covered exactly, but
    /// those characters don't appear at the start of normal F# tokens.
    /// Reads at a char boundary (`byte` is a raw-token start), so
    /// `chars().next()` is safe.
    pub(super) fn next_starts_with_ident_char(&self, byte: usize) -> bool {
        let first = self.source.get(byte..).and_then(|s| s.chars().next());
        matches!(
            first,
            Some(c) if c.is_alphanumeric() || c == '_' || c == '\'',
        )
    }

    /// `true` if the next token continues the *current* application.
    /// Uses the *arg-level* starter check ([`Parser::peek_starts_app_arg`]),
    /// not the wider [`Parser::peek_is_expr_start`]: `f - x` is infix
    /// application (`App(-, f, x)`), not `f (-x)`, so bare minus-level
    /// prefixes mustn't trip an extra app step. Then two raw-stream
    /// gates:
    ///
    /// 1. If a swallowed `RParen` sits ahead of the next non-trivia
    ///    raw, the apparent expression-starter actually belongs to an
    ///    enclosing construct and committing to it would consume the
    ///    surrounding context's arg. Same pattern as
    ///    [`Parser::at_tuple_continuation`] for the outer-comma case.
    /// 2. If the LHS's last raw token was a numeric literal AND the
    ///    next non-trivia raw token starts with an `ident_char` and
    ///    is adjacent (no whitespace gap), refuse: FCS's lex.fsl
    ///    (`(int | xint | float) ident_char+`, line 515) treats this
    ///    as a single malformed numeric literal with an error. Our
    ///    lexer splits at the digit/ident boundary so we'd silently
    ///    accept `App(1, true)` for `1true` without this guard.
    ///    Restricting to ident_char followers preserves valid
    ///    applications like `1(2)` and `1"x"` (paren/string atoms),
    ///    which FCS *does* accept as App.
    pub(super) fn at_app_continuation(&self) -> bool {
        // High-precedence (adjacent) applications `f(x)` are consumed at the
        // tighter atomic level by [`Parser::parse_postfix_tail`] — both the
        // head and each argument here go through [`Parser::parse_atomic_expr`]
        // — so by the time this *whitespace*-application gate runs, any leading
        // `Virtual::HighPrecedenceParenApp` marker has already been absorbed.
        // A *malformed* one (`f(`, `f(+)`) is left unconsumed by
        // `peek_high_precedence_paren_app`'s well-formedness gate; it is a
        // `Virtual`, not an atom-starter, so `peek_starts_app_arg` returns
        // `false` below and we stop — the marker falls through to the outer
        // decl-loop's ERROR placeholder, exactly as before.
        if !self.peek_starts_app_arg() {
            return false;
        }
        let next_non_trivia = self
            .raw_tokens
            .iter()
            .skip(self.raw_pos)
            .find_map(|(res, span)| match res {
                Ok(tt) => raw_significant(tt).map(|t| (Ok(t), span)),
                Err(e) => Some((Err(e), span)),
            });
        match next_non_trivia {
            // Swallowed closer: a `)` (paren expr) or `}` (computation expr)
            // stripped from the filtered stream marks the end of the body, so
            // a following atom is the enclosing expression's, not an argument
            // of this one (`( f ) x`, `{ id } 2`, `f { 1 } x`). Recovered
            // downstream by the body's `bump_swallowed_closer`.
            Some((Ok(Token::RParen | Token::RBrace), _)) => return false,
            // A body-trailing *operator* immediately before a swallowed closer
            // (`(1 +) x`, `(f &) x`): `peek_starts_app_arg` admits the bare
            // operator (via `op_is_adjacent_prefix`), and parsing it as an
            // operator-value would drain the swallowed `)` / `}` to reach the
            // post-closer token. The operator has no operand, so it is not an
            // argument — decline, leaving it (and the closer) for the body's
            // recovery. The token set is exactly `op_is_adjacent_prefix`'s
            // eligible operators (`Op(_)` plus the address-of `&` / `&&`, which
            // are dedicated tokens, not `Op`); a *complete* operand before the
            // closer (the `x` of `(f x)`) is none of these, so this never fires
            // for well-formed bodies. Cons / infix / cast operators take their
            // own gates in `peek_*_continuation`.
            Some((Ok(Token::Op(_) | Token::Amp | Token::AmpAmp), op_span))
                if self.op_rhs_is_swallowed_closer(op_span.end) =>
            {
                return false;
            }
            Some((_, next_span))
                if self.prev_raw_is_adjacent_numeric_atom(next_span.start)
                    && self.next_starts_with_ident_char(next_span.start) =>
            {
                return false;
            }
            _ => {}
        }
        true
    }

    /// `true` if the previous non-trivia raw token is a bare numeric
    /// literal whose end equals `next_start` (i.e. the next non-trivia
    /// raw token is immediately adjacent — no whitespace, no comment).
    /// Used by [`Parser::at_app_continuation`] to mirror FCS's
    /// malformed-numeric rule at the parser layer. Inspecting raw
    /// tokens (not the green tree) keeps the check cheap and means
    /// `(1)` — whose last raw is `)` — is correctly treated as a
    /// non-numeric atom even though it wraps a numeric literal.
    ///
    /// The [`Token::IntDotDot`] token (`<digits>..`, e.g. the `1..` of a range)
    /// is deliberately **excluded**: it ends in `..`, so its digits are never
    /// directly adjacent to the following token — the malformed rule
    /// (`(int | xint | float) ident_char+`, *no separator*) cannot apply. A
    /// genuine glued literal (`1true`, `123abc`) lexes as a bare `Int`/`Float`
    /// plus an ident, never as `IntDotDot`. Including it false-flagged the step
    /// literal of an integer stepped range (`1..2..10`), where two `IntDotDot`s
    /// sit adjacent in the raw stream (the second only partially consumed when
    /// this runs).
    pub(super) fn prev_raw_is_adjacent_numeric_atom(&self, next_start: usize) -> bool {
        let prev = self.raw_tokens[..self.raw_pos]
            .iter()
            .rev()
            .find_map(|(res, span)| match res {
                Ok(tt) => raw_significant(tt).map(|t| (t, span)),
                Err(_) => None,
            });
        let Some((tok, span)) = prev else {
            return false;
        };
        if span.end != next_start {
            return false;
        }
        matches!(
            tok,
            Token::Int(_)
                | Token::IntSuffixed(_)
                | Token::XInt(_)
                | Token::XIntSuffixed(_)
                | Token::XIEEE32(_)
                | Token::XIEEE64(_)
                | Token::BigNum(_)
                | Token::Decimal(_)
                | Token::Float32(_)
                | Token::Float64(_)
        )
    }
}
