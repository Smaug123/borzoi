//! Type productions: tuple/function/applied/atomic types, anonymous record
//! types, type-constructor application power, and the type-start lookahead.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubtypeShorthandHead {
    Typar(SyntaxKind),
    Anon,
}

impl<'src> Parser<'src> {
    /// Entry into the type grammar — `SynType`-equivalent productions.
    /// Layered to match FCS's `typ → tupleType RARROW typ` precedence
    /// (`pars.fsy:6215`): `parse_tuple_type` handles the atomic and
    /// optional `*`-separated segments, then this function conditionally
    /// wraps the result in a [`SyntaxKind::FUN_TYPE`] when the next
    /// non-trivia *raw* token is `->`. Right-recursive on the arrow,
    /// so `int -> int -> int` nests as `Fun(int, Fun(int, int))`;
    /// `*` binds tighter than `->`, so `int * int -> int` projects as
    /// `Fun(Tuple(int, int), int)`. Postfix/prefix application, array,
    /// hash constraint, and anon record land in subsequent 7.x
    /// sub-phases — see `docs/parser-plan.md`.
    ///
    /// Drains trivia before opening the type node, so leading whitespace
    /// (between `:` and the type) lands as a sibling of the type rather
    /// than as a first child, matching the trivia model used elsewhere.
    pub(super) fn parse_type(&mut self) {
        self.parse_type_impl(false);
    }

    /// As [`Self::parse_type`], but in FCS's `topType` context (`pars.fsy:6055`):
    /// a value / member / delegate *signature* type, where each arrow-argument and
    /// tuple-element position admits a labelled parameter `[?]ident : <appType>`
    /// (`SynType.SignatureParameter`, phase 10.12b). The flag propagates to the
    /// arrow return and tuple elements but **not** into parens / generic arguments
    /// (those reset to the general `typ` grammar), so a named param is recognised
    /// only where `topType` allows it.
    pub(super) fn parse_top_type(&mut self) {
        self.parse_type_impl(true);
    }

    fn parse_type_impl(&mut self, top: bool) {
        // Depth-guarded: the type recursion chokepoint. Nested generics
        // (`list<list<…>>`), tuple types, and the right-associative arrow
        // (`int -> int -> …`, recursed below) all re-enter here, so bounding
        // this one function bounds type nesting. The shared counter is the same
        // one the expression and pattern chokepoints use, so a deeply nested
        // type reached from an expression (`(e : T)`) is bounded jointly.
        self.with_depth(|p| {
            // Check the *raw* stream first, before draining: a swallowed `)`
            // sits between `raw_pos` and the next filtered token, so draining
            // would consume it as ERROR and the filtered lookahead could then
            // claim a type-starter that lives outside the surrounding parens
            // (`(x : ) y` — see `in_paren_missing_type_does_not_eat_outer_rparen`).
            // Gate type-acceptance on the next non-trivia raw being a
            // type-starter so the recovery error fires at the in-paren
            // boundary without stealing tokens past it.
            //
            // [`Parser::peek_starts_type`] also admits a *leading* `/` (FCS's
            // `INFIX_STAR_DIV_MOD_OP tupleOrQuotTypeElements`, `pars.fsy:6262`,
            // phase 10.9) — the no-numerator measure tuple `float</s>` / bare
            // `(x : /s)` → `Tuple([Slash, Type(s)])`, which the atomic-level
            // `peek_starts_type_or_anon_recd` (correctly) rejects but
            // [`Parser::parse_tuple_type`] owns. (A leading `*` stays an error: FCS
            // reports "Expecting type" and recovers with a `FromParseError`, phase
            // 11.)
            //
            // In `top` context an optional parameter (`?x: int`) starts with `?`,
            // and an *attributed* parameter (`[<A>] x: int`) with `[<`; neither is
            // a general type-starter, so admit both via the sig-param lookahead so
            // the head element reaches `parse_signature_parameter`.
            if !(p.peek_starts_type()
                || (top && (p.peek_is_signature_parameter() || p.peek_at_type_attribute())))
            {
                let span = p
                    .peek()
                    .map(|(_, span)| span.clone())
                    .unwrap_or_else(|| p.source.len()..p.source.len());
                p.errors.push(ParseError {
                    message: "expected type".to_string(),
                    span,
                });
                return;
            }
            if let Some((_, span)) = p.peek() {
                let start = span.start;
                p.drain_raw_up_to(start);
            }
            // Capture a checkpoint *after* the trivia drain so a subsequent
            // arrow wraps only the tuple-or-atomic LHS and the arrow/return
            // inside `FUN_TYPE`. Leading trivia between `:` and the type
            // stays a sibling of the type node — matching the invariant the
            // other 7.x atomic arms rely on (so `Type::syntax().text_range()`
            // is consistent across `LongIdent`, `Var`, `Paren`, `Tuple`,
            // and `Fun`).
            let cp = p.builder.checkpoint();
            p.parse_tuple_type_impl(top);

            // Gate the arrow continuation on the next non-trivia *raw*
            // token: a LexFilter-swallowed `)` between the LHS and the next
            // filtered token must not be looked past (otherwise
            // `(f : int ->) y` would treat the outer ident as the
            // return-type starter). Same shape as the raw-stream gates on
            // the dotted-loop and `parse_atomic_type`'s LPAREN arm.
            if p.next_non_trivia_raw_at_pos()
                .is_some_and(|t| matches!(t, Token::RArrow))
            {
                p.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::FUN_TYPE));
                p.bump_into(SyntaxKind::RARROW_TOK);
                // Recursive call — handles the return type, including
                // further arrows (right-associative). `top` propagates: the return
                // type is itself a `topType`, so its arguments may be labelled.
                p.parse_type_impl(top);
                p.builder.finish_node();
            }
        });
    }

    /// Tuple-type layer (`tupleType` in `pars.fsy:6243`). Parses one
    /// atomic type, then if the next non-trivia *raw* token is the
    /// `*` operator, retroactively wraps the LHS atomic and the
    /// subsequent `*`-separated atomics inside a
    /// [`SyntaxKind::TUPLE_TYPE`] node. The path is *flat*: a single
    /// node contains every atomic and every `STAR_TOK` separator in
    /// source order, mirroring FCS's `SynTupleTypeSegment` list shape.
    ///
    /// Phase 7.4 covers the `*` separator and the non-struct form; phase
    /// 10.9 adds the `/` (Slash) segment — `INFIX_STAR_DIV_MOD_OP` with the
    /// op text exactly `/` (`pars.fsy:6262-6285`), used by the
    /// unit-of-measure division form `float<1/s>` to fold a leading
    /// `StaticConstant` and a divisor into a `Tuple` whose path carries a
    /// [`SynTupleTypeSegment.Slash`](SyntaxKind::SLASH_TOK). `struct (T * U)`
    /// is still deferred.
    ///
    /// A *leading* `/` (FCS's `INFIX_STAR_DIV_MOD_OP tupleOrQuotTypeElements`,
    /// `pars.fsy:6262`) opens a measure tuple with no numerator (`float</s>` →
    /// `Tuple([Slash, Type(s)])`, no leading `Type` segment); detected before
    /// the head type and folded into the same separator loop. (The caller
    /// [`Parser::parse_type`] is responsible for letting the leading `/` past
    /// its atomic-level type-start gate.)
    ///
    /// The op-text gates (`Token::Op("*")` / `Token::Op("/")`) are exact:
    /// `**`, `//`, `*+`, etc. reach the parser as longer `Op` tokens by the
    /// lexer's longest-match rule and never trigger the tuple loop here. FCS
    /// rejects the other `INFIX_STAR_DIV_MOD_OP` spellings (`%`, `mod`, …) in
    /// a tuple type, so we likewise loop only on `*` / `/`. The raw-stream
    /// gate (rather than filtered `peek`) preserves the same swallowed-`)`
    /// invariant the arrow layer relies on.
    fn parse_tuple_type_impl(&mut self, top: bool) {
        let cp = self.builder.checkpoint();
        // A leading `/` has no head type; skip straight to the separator loop
        // (which bumps the `/` as the first `SLASH_TOK` segment). Otherwise
        // parse the head type, then bail unless a `*` / `/` separator follows.
        let leading_slash = self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Op("/")));
        if !leading_slash {
            self.parse_top_app_type_element(top);
            if !self
                .next_non_trivia_raw_at_pos()
                .is_some_and(|t| matches!(t, Token::Op("*" | "/")))
            {
                return;
            }
        }

        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TUPLE_TYPE));
        self.sweep_tuple_type_separators(top);
        self.builder.finish_node();
    }

    /// The `( STAR | SLASH ) topAppType` separator loop shared by the flat
    /// [`Self::parse_tuple_type`] and the parenthesised
    /// [`Self::parse_struct_tuple_type`]: bumps each `*` / `/` separator and the
    /// element after it into the *currently open* `TUPLE_TYPE` node, returning
    /// the number of separators consumed (so the struct form can enforce its
    /// ≥2-element rule). The caller has already parsed the head element.
    fn sweep_tuple_type_separators(&mut self, top: bool) -> usize {
        let mut separators = 0usize;
        while let Some(sep_kind) = self.next_non_trivia_raw_at_pos().and_then(|t| match t {
            Token::Op("*") => Some(SyntaxKind::STAR_TOK),
            Token::Op("/") => Some(SyntaxKind::SLASH_TOK),
            _ => None,
        }) {
            self.bump_into(sep_kind);
            separators += 1;
            // After the `*` / `/`, gate the next atomic on the raw stream
            // too — a swallowed `)` immediately after the separator would
            // leave the filtered next token as an outer atomic that must not
            // be absorbed (`(x : int *) y`). The error-on-miss path
            // mirrors `parse_type`'s opening gate so the recovery error
            // fires at the in-paren boundary. The gate accepts the full
            // `atomTypeOrAnonRecdType` layer because
            // [`Parser::parse_app_type`] (called below) dispatches to
            // [`Parser::parse_anon_recd_type`] when the head is `{|` /
            // `struct {|`.
            // In `top` context a tuple element may itself be a labelled / optional
            // parameter (`x: int * ?y: int`), whose leading `?` is not a general
            // type-starter, or an *attributed* element (`x: int * [<A>] y: int`),
            // whose leading `[<` is not either; admit both via the sig-param
            // lookahead.
            if !(self.peek_starts_type_or_anon_recd()
                || (top && (self.peek_is_signature_parameter() || self.peek_at_type_attribute())))
            {
                let span = self
                    .peek()
                    .map(|(_, span)| span.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected type after tuple-type separator".to_string(),
                    span,
                });
                break;
            }
            self.parse_top_app_type_element(top);
        }
        separators
    }

    /// Parse a struct-tuple type `struct ( T1 * T2 [* …] )` (FCS's `STRUCT
    /// LPAREN … RPAREN` → `SynType.Tuple(isStruct = true, [Type; Star; Type;
    /// …])`, `pars.fsy`). Unlike a flat `T1 * T2` tuple, the `struct` marker and
    /// the parens belong to *this* node — there is no `Paren` wrapper — so the
    /// `STRUCT_TOK`, `LPAREN_TOK`, element types, `STAR_TOK`/`SLASH_TOK`
    /// separators and the (LexFilter-swallowed) `RPAREN_TOK` all sit directly
    /// under one [`SyntaxKind::TUPLE_TYPE`], and [`crate::syntax::TupleType::is_struct`]
    /// reads the marker. The differential normaliser elides the `struct`/parens
    /// tokens, leaving the same flat `path` a non-struct tuple projects, plus
    /// `is_struct = true`.
    ///
    /// FCS requires ≥2 elements (`struct (int)` / `struct ()` are parse errors),
    /// so a missing `*`/`/` after the first element is reported (the
    /// inner is `top = false` — the parens reset to the general `typ`, with no
    /// labelled parameters, like an ordinary paren type). Caller has verified
    /// [`Self::peek_starts_struct_tuple_type`] (`struct` then `(`).
    pub(super) fn parse_struct_tuple_type(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TUPLE_TYPE));
        self.bump_into(SyntaxKind::STRUCT_TOK);
        // The opening `(` is a real filtered token (only the closing `)` is
        // swallowed); `bump_into` drains the `struct`/`(` trivia before it.
        self.bump_into(SyntaxKind::LPAREN_TOK);
        // Check the *raw* stream (via the raw-aligned `peek_starts_type_or_anon_recd`)
        // *before* draining: LexFilter has swallowed the matching `)`, so for an
        // empty `struct ()` the filtered peek lands past the close (`(x : struct ())
        // y` would otherwise drain the closers as `ERROR` and steal the outer `y`
        // as the first element). Gate first, drain only once a type is known to
        // start — mirroring the `PAREN_TYPE` arm. The parens reset the
        // labelled-parameter context, so `top = false`.
        if self.peek_starts_type_or_anon_recd() {
            // Drain raw trivia between `(` and the first element so it attaches to
            // `TUPLE_TYPE` rather than landing inside the first element node.
            if let Some((_, next_span)) = self.peek() {
                let start = next_span.start;
                self.drain_raw_up_to(start);
            }
            self.parse_top_app_type_element(false);
            // FCS's production is `STRUCT LPAREN appType STAR tupleOrQuotTypeElements`
            // — the *first* separator must be `*` (a leading `/` measure-divide
            // `struct (int / s)` is a parse error), though `/` is valid in the tail
            // (`struct (int * s / t)`). Peek the first separator before the sweep
            // (which is lossless and accepts both for the tail).
            let first_sep_is_slash =
                matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Op("/")));
            let separators = self.sweep_tuple_type_separators(false);
            if separators == 0 {
                // FCS's struct tuple needs ≥2 elements; `struct (int)` is a parse
                // error ("Unexpected symbol ')'"). Mirror it (both sides reject),
                // staying lossless — the cursor is left for the `)` bump below.
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "a struct tuple type needs at least two elements".to_string(),
                    span,
                });
            } else if first_sep_is_slash {
                // `struct (int / s)` — `/` is not a valid *first* separator (FCS
                // rejects it); the sweep consumed it for losslessness, so just flag.
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "a struct tuple type's first separator must be `*`".to_string(),
                    span,
                });
            }
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a type inside `struct (…)`".to_string(),
                span,
            });
        }
        // The closing `)` is swallowed by the lex-filter, recovered off the raw
        // stream like a paren type's.
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        self.builder.finish_node(); // TUPLE_TYPE
    }

    /// Parse one `topAppType` element (`pars.fsy:6125`) — a tuple element or the
    /// arrow LHS. In `top` (signature) context a labelled argument
    /// `[?]ident : <appType>` or an *attributed* element (`[<A>] x : <appType>`,
    /// `[<A>] <appType>`) becomes a [`SyntaxKind::SIGNATURE_PARAMETER_TYPE`]
    /// (phase 10.12b); otherwise (an unnamed, unattributed element) it is the
    /// ordinary `appTypeCanBeNullable`.
    fn parse_top_app_type_element(&mut self, top: bool) {
        if top && (self.peek_is_signature_parameter() || self.peek_at_type_attribute()) {
            self.parse_signature_parameter();
        } else {
            self.parse_app_type_can_be_nullable();
        }
    }

    /// `true` iff the cursor is at an attribute-list opener (`[<`,
    /// [`Token::LBrackLess`]) — in a `top` signature type this can only begin an
    /// attributed parameter element (`[<A>] x : int`), so it routes to
    /// [`Self::parse_signature_parameter`]. (Attributes are not a general type
    /// start, so this is consulted only at the `top`-context gate sites.)
    ///
    /// Gated on *both* streams, mirroring [`Self::peek_is_signature_parameter`]'s
    /// dual discipline: the **filtered** cursor must be the `[<` (rejecting a
    /// layout virtual the raw scan would look past), and the next non-trivia
    /// **raw** token must be `[<` too — so a LexFilter-swallowed `)` sitting
    /// between the cursor and the `[<` (an incomplete `(member M : ) [<A>] int`)
    /// surfaces as a raw `RParen` and blocks the match, keeping the gates from
    /// draining past the closer and stealing an attributed type from the outer
    /// context (the hazard the `peek_starts_type` raw gate guards).
    pub(super) fn peek_at_type_attribute(&self) -> bool {
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) && matches!(self.next_non_trivia_raw_at_pos(), Some(Token::LBrackLess))
    }

    /// `true` iff the cursor is at a *labelled* `topAppType` parameter —
    /// `ident :` (named) or `? ident :` (optional) — where the name is a *single*
    /// identifier (FCS requires `SynLongIdent([id])`) immediately followed by a
    /// plain `:` ([`Token::Colon`] exactly, so `:>` / `:?>` / `::` do not match).
    /// Only meaningful in `top` context. (A *leading attribute* on the parameter
    /// is detected separately by [`Self::peek_at_type_attribute`]; this checks the
    /// label, which `parse_signature_parameter` re-tests after the attribute run.)
    pub(super) fn peek_is_signature_parameter(&self) -> bool {
        // The *filtered* cursor must itself be the label head (`?` / ident), not a
        // layout virtual the raw-stream scan below would look past. Without this, a
        // dedented sibling after a dangling `val f :` (`val f :`⏎`g: int`, where an
        // `OBLOCKEND`/`OBLOCKSEP` sits at the cursor) would be seen as `g :` on the
        // raw stream and stolen as a label. The valid offside continuation
        // (`val f :`⏎`  x: int`) keeps the ident at the cursor (no intervening
        // virtual), so it is unaffected.
        if !matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::QMark | Token::Ident(_) | Token::QuotedIdent(_)
                )),
                _,
            ))
        ) {
            return false;
        }
        match self.next_non_trivia_raw_at_pos() {
            // `? ident :` — an optional parameter.
            Some(Token::QMark) => {
                matches!(
                    self.nth_significant_raw_at_pos(1),
                    Some(Token::Ident(_) | Token::QuotedIdent(_))
                ) && matches!(self.nth_significant_raw_at_pos(2), Some(Token::Colon))
            }
            // `ident :` — a named parameter.
            Some(Token::Ident(_) | Token::QuotedIdent(_)) => {
                matches!(self.nth_significant_raw_at_pos(1), Some(Token::Colon))
            }
            _ => false,
        }
    }

    /// Parse a signature parameter `[<attrs>]? [?]ident : <appType>` (or an
    /// unnamed `[<attrs>] <appType>`) into a
    /// [`SyntaxKind::SIGNATURE_PARAMETER_TYPE`] (FCS's `SynType.SignatureParameter`,
    /// phase 10.12b). Shape `[ATTRIBUTE_LIST*] [QMARK_TOK?] [IDENT_TOK COLON_TOK]?
    /// <appType>`: a leading attribute run (FCS's `attributes`, field 0) is homed
    /// here as [`SyntaxKind::ATTRIBUTE_LIST`] children; the optional `?` flags
    /// `isOptional`; the `IDENT_TOK` is the parameter name (absent → FCS `id =
    /// None`, the unnamed-but-attributed form `[<A>] int`); and the value type
    /// after the colon (or, unlabelled, directly after the attributes) is an
    /// `appTypeCanBeNullable` (the arrow / `*` continue at the enclosing
    /// `topType` / `topTupleType` level). Caller has verified
    /// [`Self::peek_is_signature_parameter`] or [`Self::peek_at_type_attribute`].
    fn parse_signature_parameter(&mut self) {
        self.builder.start_node(FSharpLang::kind_to_raw(
            SyntaxKind::SIGNATURE_PARAMETER_TYPE,
        ));
        // A leading attribute run (`[<InlineIfLambda>] k : …`) — FCS's
        // `SynType.SignatureParameter.attributes`. Parse one `[< … >]` list at a
        // time, re-gating each on the raw-aligned [`Self::peek_at_type_attribute`]
        // (rather than the generic [`Parser::parse_attribute_lists`], whose
        // *filtered-only* continuation check would cross a LexFilter-swallowed `)`
        // in this parenthesised SRTP-member-sig position and fold an outer
        // `[<…>]` / type past the closer — `(member M : [<A>] ) [<B>] int`). The
        // raw gate sees the swallowed `)` as a raw `RParen` and stops the run,
        // leaving the closer for the outer context. Between lists / before the
        // parameter, drain FCS's `opt_OBLOCKSEP` `Virtual::BlockSep` (the offside
        // `[<A>]⏎[<B>]` and `[<A>]⏎ x: int` layouts) as zero-width ERRORs so the
        // next gate lands on a real token.
        while self.peek_at_type_attribute() {
            self.parse_attribute_list();
            while matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            ) {
                self.bump_into(SyntaxKind::ERROR);
            }
        }
        // The optional `[?]ident :` label. Present for a named/optional parameter;
        // absent for an unnamed-but-attributed one (`[<A>] int`), where the value
        // type follows the attributes directly (FCS `id = None`).
        if self.peek_is_signature_parameter() {
            if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::QMark)) {
                self.bump_into(SyntaxKind::QMARK_TOK);
            }
            self.bump_into(SyntaxKind::IDENT_TOK);
            self.bump_into(SyntaxKind::COLON_TOK);
        }
        // Guard the value-type parse: an incomplete `x: -> int` / `?x:` (a token
        // that is not an app-type starter after the `:`) would otherwise reach
        // `parse_atomic_type`'s `unreachable!`. Mirror the recovery the arrow/tuple
        // gates use — record a clean "expected type" error and leave the cursor for
        // the enclosing loop, so an in-progress edit stays lossless (no panic).
        if self.peek_starts_type_or_anon_recd() {
            self.parse_app_type_can_be_nullable();
        } else {
            let span = self
                .peek()
                .map(|(_, span)| span.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a type after the parameter `:`".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // SIGNATURE_PARAMETER_TYPE
    }

    /// Nullable-type layer (`appTypeCanBeNullable` in `pars.fsy:6357`):
    /// `appTypeWithoutNull BAR_JUST_BEFORE_NULL NULL`, projecting to
    /// `SynType.WithNull(inner, ambivalent: false, range, { BarRange })`.
    /// Sits between [`Parser::parse_tuple_type`] (the `*` layer above) and
    /// [`Parser::parse_app_type`] (postfix array/app below), so the inner
    /// type binds the whole postfix run: `int list | null` parses as
    /// `WithNull(App(list, [int]))`, and `string | null * int` as
    /// `Tuple([WithNull(string), int])`.
    ///
    /// FCS's `BAR_JUST_BEFORE_NULL` is the lexfilter relabel of a plain
    /// `BAR` when the `NullnessChecking` feature is on *and* the next token
    /// is `NULL` (`LexFilter.fs:2613`). We have no lexfilter relabel, so we
    /// reproduce the condition directly, with the raw-stream-first gating
    /// the dot-chain and tuple layers use:
    ///
    /// 1. The next *raw* non-trivia token must be an exact `|`
    ///    ([`Token::Bar`] — the longest-match lexer keeps `|>` as
    ///    `Op("|>")` and `||` as [`Token::BarBar`], so neither reaches
    ///    here). Gating on the raw stream (not just the filtered `peek`)
    ///    keeps the swallowed-`)` invariant: a LexFilter-swallowed close
    ///    paren of an enclosing `(e : T)` sits between the inner type and
    ///    the filtered cursor, so `peek` alone could already point at an
    ///    *outer* `|`. The raw token after the inner type is then the `)`,
    ///    not the `|`, so we correctly decline to wrap.
    /// 2. The filtered cursor must also be at that `|` (no intervening
    ///    filtered virtual), so `bump_into` consumes the right token.
    /// 3. `NULL` must immediately follow the `|` on the raw stream (rules
    ///    out a swallowed *real* token — e.g. a `)` — between them).
    /// 4. The *immediately following filtered token* must also be `NULL`,
    ///    so a LexFilter layout virtual between the `|` and `null` (which
    ///    lives in the filtered stream, not the raw one, so check 3 can't
    ///    see it) is never consumed as an empty `NULL_TOK`. Not reachable
    ///    through today's typed-paren / anon-recd surface — both suppress
    ///    offside between `|` and `null` — but `parse_type` gains
    ///    offside-live callers in later phases (let-binding / signature
    ///    annotations), where it would be; this mirrors the per-bump
    ///    filtered confirms the dot-chain path already uses.
    pub(super) fn parse_app_type_can_be_nullable(&mut self) {
        let cp = self.builder.checkpoint();
        // The `appTypeWithoutNull` beneath the nullable layer. When it is the
        // subtype shorthand it has already absorbed any `| null` into its RHS
        // `typ`, so there is no suffix left for *this* layer to wrap.
        if self.parse_app_type_without_null_at(cp) {
            return;
        }

        // Gate on the *raw* stream first, exactly as the tuple `*` and
        // dot-chain layers do. A LexFilter-swallowed `)` (the close paren
        // of an enclosing `(e : T)`) sits between the inner type and the
        // filtered cursor, so `peek` alone can already point at an *outer*
        // `|` past that `)`. Requiring the next raw non-trivia token to be
        // the `|` rejects `(x : string) | null`: the raw token after
        // `string` is `)`, not `|`, so we don't wrap (and don't drain the
        // `)` as ERROR while absorbing the outer `| null`).
        if !self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Bar))
        {
            return;
        }
        // Confirm the filtered cursor itself is at the `|` (no intervening
        // filtered virtual the raw lookahead skipped past) so `bump_into`
        // consumes the right token.
        let Some((Ok(FilteredToken::Raw(Token::Bar)), bar_span)) = self.peek().cloned() else {
            return;
        };
        if !matches!(
            self.next_non_trivia_raw_after(bar_span.end),
            Some(Token::Null)
        ) {
            return;
        }
        // The token `bump_into(NULL_TOK)` will consume is `filtered_tokens
        // [pos + 1]` (the bar is at `pos`; `bump_into` advances by exactly
        // one). Confirm it is the `null` itself, not a layout virtual the
        // raw checks above can't see.
        if !matches!(
            self.filtered_tokens.get(self.pos + 1),
            Some((Ok(FilteredToken::Raw(Token::Null)), _)),
        ) {
            return;
        }

        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::WITH_NULL_TYPE));
        self.bump_into(SyntaxKind::BAR_TOK);
        self.bump_into(SyntaxKind::NULL_TOK);
        self.builder.finish_node();
    }

    /// `appTypeWithoutNull` (`pars.fsy:6371`) — the layer directly beneath
    /// [`Parser::parse_app_type_can_be_nullable`]. Returns `true` iff it consumed
    /// the *subtype shorthand*, which the caller needs to know because that form
    /// swallows its own `| null`.
    ///
    /// It is the postfix array/app run ([`Parser::parse_app_type`]) **plus** FCS's
    /// `typar COLON_GREATER typ` / `UNDERSCORE COLON_GREATER typ`
    /// (`pars.fsy:6389-6390`) — the `'a :> T` / `_ :> T` subtype-constraint
    /// shorthand. The typar form folds to
    /// `SynType.WithGlobalConstraints(Var 'a, [WhereTyparSubtypeOfType('a, T)])`;
    /// the anonymous form folds to `SynType.HashConstraint(T)`, like `#T`, with the
    /// `_ :>` surface preserved as tokens in the hash node. The shorthand is a whole
    /// `appTypeWithoutNull` whose LHS is a *bare* typar/underscore, so it lives here
    /// (below the tuple `*` / arrow / nullable layers) and composes with them:
    /// `'a * 'b :> T` is `'a * ('b :> T)`, `('a :> T) -> u` keeps the constraint on
    /// `'a`. Its RHS is a full `typ`, so a `| null` suffix folds *into* it
    /// (`'T :> IDisposable | null` → subtype of `WithNull(IDisposable)`) rather than
    /// wrapping the shorthand — hence the `true` return.
    ///
    /// Callers that want the production *itself* (rather than as the nullable
    /// layer's base) go through [`Parser::parse_app_type_without_null`]; the SRTP
    /// member constraint's `typeAlts` operand is one, and it is why this is factored
    /// out at all: reaching for the bare `parse_app_type` there silently dropped the
    /// subtype shorthand, since `appTypeWithoutNull` is strictly larger than the
    /// postfix run.
    ///
    /// `cp` is the builder checkpoint taken *before* the type, so the shorthand's
    /// node can wrap what it parses.
    fn parse_app_type_without_null_at(&mut self, cp: rowan::Checkpoint) -> bool {
        let Some(head) = self.peek_is_bare_subtype_shorthand() else {
            self.parse_app_type();
            return false;
        };
        match head {
            SubtypeShorthandHead::Typar(sigil_kind) => {
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::CONSTRAINED_TYPE));
                self.parse_var_type(sigil_kind);
                self.bump_into(SyntaxKind::COLON_GREATER_TOK);
                self.parse_type();
                self.builder.finish_node(); // CONSTRAINED_TYPE
            }
            SubtypeShorthandHead::Anon => {
                self.builder.start_node_at(
                    cp,
                    FSharpLang::kind_to_raw(SyntaxKind::HASH_CONSTRAINT_TYPE),
                );
                self.bump_into(SyntaxKind::UNDERSCORE_TOK);
                self.bump_into(SyntaxKind::COLON_GREATER_TOK);
                self.parse_type();
                self.builder.finish_node(); // HASH_CONSTRAINT_TYPE
            }
        }
        true
    }

    /// `appTypeWithoutNull` (`pars.fsy:6371`) as a production in its own right —
    /// the postfix app run *and* the `'a :> T` / `_ :> T` subtype shorthand, but
    /// **not** the `| null` suffix (that is `appTypeCanBeNullable`, one layer up:
    /// [`Parser::parse_app_type_can_be_nullable`]). Used where FCS's grammar names
    /// this exact non-terminal — the SRTP member constraint's `typeAlts` operand,
    /// where a `| null` alternative is an FCS parse error.
    pub(super) fn parse_app_type_without_null(&mut self) {
        let cp = self.builder.checkpoint();
        self.parse_app_type_without_null_at(cp);
    }

    /// Detect the `'a :> T` / `_ :> T` subtype shorthand at the cursor. The `:>`
    /// is required on *both* the filtered and the raw stream after the bare head,
    /// so a LexFilter-swallowed `)` before an enclosing-expression upcast
    /// (`(x: 'a) :> T`, which collapses to `'a :>` in the filtered stream) is not
    /// mistaken for a type-level subtype constraint.
    fn peek_is_bare_subtype_shorthand(&self) -> Option<SubtypeShorthandHead> {
        let sigil_kind = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Quote)), _)) => SyntaxKind::QUOTE_TOK,
            Some((Ok(FilteredToken::Raw(Token::Op("^"))), _)) => SyntaxKind::HAT_TOK,
            Some((Ok(FilteredToken::Raw(Token::Underscore)), underscore_span)) => {
                let colon_idx = self.next_non_trivia_filtered_index_after(self.pos)?;
                let filtered_cg = matches!(
                    self.filtered_tokens.get(colon_idx),
                    Some((Ok(FilteredToken::Raw(Token::ColonGreater)), _))
                );
                let raw_cg = matches!(
                    self.next_non_trivia_raw_after(underscore_span.end),
                    Some(Token::ColonGreater)
                );
                return (filtered_cg && raw_cg).then_some(SubtypeShorthandHead::Anon);
            }
            _ => return None,
        };
        let ident_idx = self.next_non_trivia_filtered_index_after(self.pos)?;
        let ident_end = match self.filtered_tokens.get(ident_idx) {
            Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), s)) => s.end,
            _ => return None,
        };
        let colon_idx = self.next_non_trivia_filtered_index_after(ident_idx)?;
        let filtered_cg = matches!(
            self.filtered_tokens.get(colon_idx),
            Some((Ok(FilteredToken::Raw(Token::ColonGreater)), _))
        );
        let raw_cg = matches!(
            self.next_non_trivia_raw_after(ident_end),
            Some(Token::ColonGreater)
        );
        (filtered_cg && raw_cg).then_some(SubtypeShorthandHead::Typar(sigil_kind))
    }

    /// Postfix type-application layer (`appTypeWithoutNull` in
    /// `pars.fsy:6371` — specifically the `appTypeWithoutNull
    /// appTypeConPower` alternative at `pars.fsy:6378`). Parses one
    /// atomic type, then while the next non-trivia *raw* token starts a
    /// postfix-head ([`raw_starts_postfix_app_head`] — `path` or
    /// `typar`, mirroring FCS's `appTypeConPower → appTypeCon` head
    /// restriction), parses another atomic and retroactively wraps the
    /// running result as [`SyntaxKind::APP_TYPE`].
    ///
    /// Left-associative by construction: every wrap starts at the *same*
    /// `cp` (captured before the first atomic), so `int list option`
    /// nests as `App(App(int, list), option)` — the outer `App`'s first
    /// `Type` child is the previous inner `App`, not a flat sibling list.
    /// Mirrors FCS's `App(name, None, [arg], [], None, true, range)`
    /// shape; the rowan node keeps children in source order `[arg,
    /// head]` with the head's [`SyntaxKind::APP_TYPE`] accessor reading
    /// the *last* `Type` child.
    ///
    /// The prefix surface form `Foo<int, …>` (`pars.fsy:6618`
    /// `HIGH_PRECEDENCE_TYAPP typeArgsActual`) is **not** wrapped here —
    /// it sits one layer below in [`Parser::parse_atomic_type`], because
    /// FCS treats it as an `appTypeCon` alternative inside `atomType`
    /// (`pars.fsy:6534-6539` → `pars.fsy:6618`). Wrapping it here would
    /// place hash constraints / paren-types at the wrong layer relative
    /// to the prefix-app — `#Foo<int>` must produce
    /// `HashConstraint(App(Foo, [int]))`, not
    /// `App(HashConstraint(Foo), [int])`. Mixed cases like
    /// `Foo<int> list` still nest as `App(App(Foo, <int>), list)`
    /// because the postfix loop here wraps whatever atomic
    /// `parse_atomic_type` returned (including the inner `APP_TYPE`).
    ///
    /// Layer placement: sits between `parse_tuple_type` (looser, `*`)
    /// and `parse_atomic_type` (tighter), so `*` and `->` see the
    /// already-built `App` as a single LHS — `int list * string list`
    /// projects as `Tuple(App(int,list), App(string,list))` and `int
    /// -> int list` as `Fun(int, App(int, list))`.
    ///
    /// Measure-power `appTypeCon INFIX_AT_HAT_OP
    /// atomicRationalConstant` (`pars.fsy:6344`) and the
    /// `typ EQUALS typ` named-static-constant arg form
    /// (`pars.fsy:6668`) are deferred to later phases.
    ///
    /// Phase 7.7 also handles the array-suffix surface form `T[]`,
    /// `T[,]`, `T[,,]`, … (`pars.fsy:6371-6376` + `pars.fsy:6397-…`).
    /// Two grammar arms — `appTypeWithoutNull arrayTypeSuffix` (when
    /// `T` is not IDENT-adjacent, e.g. `(int)[]`) and
    /// `appTypeWithoutNull HIGH_PRECEDENCE_BRACK_APP arrayTypeSuffix`
    /// (`int[]`, where LexFilter inserts the HPBA virtual between the
    /// IDENT and the `[`) — collapse to the same `SynType.Array(rank,
    /// elementType, _)` projection, so the wrap here records both
    /// behind one [`SyntaxKind::ARRAY_TYPE`] kind. The HPBA, when
    /// present, is consumed as a zero-width `ERROR` placeholder before
    /// `LBRACK_TOK`, mirroring the prefix-HPA treatment above.
    ///
    /// Array-suffix and postfix-app live in the same `loop` since both
    /// alternatives sit at the same `appTypeWithoutNull` level in the
    /// grammar and can interleave: `int list[]` parses as
    /// `Array(rank=1, App(list, [int], postfix))` (postfix-app first,
    /// then array), and `int[] list` as
    /// `App(list, [Array(rank=1, int)], postfix)` (array first, then
    /// postfix-app). Each iteration picks the branch based on the next
    /// non-trivia raw — `LBrack` ⇒ array suffix, [`raw_starts_postfix_app_head`]
    /// ⇒ postfix-app, anything else ⇒ exit. The wraps share `cp`, so
    /// `int[][]` (a jagged array) nests as
    /// `Array(rank=1, Array(rank=1, int))` — left-associative on the
    /// FCS side (`appTypeWithoutNull arrayTypeSuffix` is left-recursive).
    pub(super) fn parse_app_type(&mut self) {
        let cp = self.builder.checkpoint();
        // `intersectionType` (`pars.fsy:6328-6335`, phase 10.10): a bare
        // `typar`/`^T` or `hashConstraint` head, *immediately* followed by `&`,
        // opens a `SynType.Intersection` rather than continuing the
        // postfix-app/measure run. Classify the head *before* parsing it — the
        // typar arm must reject a following `<` HPA prefix-app (`'T<int>`),
        // which FCS does not admit as an intersection head (`'T<int> & …` is a
        // parse error there).
        let head_intersectable = self.at_intersection_head();
        self.parse_atom_type_or_anon_recd_type();

        // Fire only when the *bare* head atom is immediately followed by `&`:
        // the post-parse `&` gate confirms the head was a single atomic (a
        // postfix-applied `#A list &` leaves `list` — not `&` — at the cursor,
        // so it correctly declines).
        if head_intersectable
            && self
                .next_non_trivia_raw_at_pos()
                .is_some_and(|t| matches!(t, Token::Amp))
        {
            self.parse_intersection_tail(cp);
            // Fall through to the shared postfix/array loop — **not** the
            // measure-power tail. FCS reduces `intersectionType` to an
            // `appTypeWithoutNull`, so the `arrayTypeSuffix` / `appTypeConPower`
            // continuations still apply (`#A & #B list` → `App(list,
            // [Intersection])`, `#A & #B[]` → `Array(Intersection)`), but
            // `powerType` is a *sibling* alternative, not a continuation, so a
            // trailing `^` is not folded onto the intersection.
        } else {
            // Measure-power on the head atom — FCS's `powerType:
            // atomTypeOrAnonRecdType ^ exp` (phase 10.8). A path / typar head
            // already consumed its `^` inside `parse_app_type_con_power`, so
            // this is a no-op there; it is what wraps the *other* atom bases
            // (`(m)^2` → `MeasurePower(Paren m, 2)`, `Foo<int>^2`, `{| … |}^2`),
            // which the postfix-loop right-hand restriction would otherwise
            // leave with a dangling `^`. Binds tighter than the postfix
            // product, so `(m)^2 kg` keeps the power inside the product's
            // factor.
            self.try_parse_measure_power_tail(cp);
        }

        loop {
            // Bail at a layout virtual: `Virtual::BlockSep` /
            // `Virtual::BlockEnd` at the filtered cursor marks a
            // structural boundary that the postfix-app / array-suffix
            // continuations must not cross. Without this gate, raw-stream
            // lookahead would skip over the virtual, see the next ident
            // (e.g. the next anon-record field name in
            // `{| F : int\n   G : string |}`), and dispatch into
            // `parse_app_type_con_power` with the virtual still parked at
            // the cursor — which trips its unreachable arm. The plain
            // cross-line continuation `int\n   list` does *not* emit a
            // BlockSep here (the continuation is past the offside), so
            // legitimate multi-line postfix-apps still work.
            if matches!(
                self.peek(),
                Some((
                    Ok(FilteredToken::Virtual(
                        Virtual::BlockSep | Virtual::BlockEnd
                    )),
                    _
                )),
            ) {
                break;
            }
            let next = self.next_non_trivia_raw_at_pos();
            if matches!(next, Some(Token::LBrack)) {
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::ARRAY_TYPE));
                // The IDENT-adjacent `name[]` arm carries a
                // `Virtual::HighPrecedenceBrackApp` between the IDENT
                // and the `[` (`LexFilter.fs:2653`, our
                // `crates/cst/src/lexfilter/mod.rs:1818`). Consume it
                // as a zero-width ERROR placeholder so the source-text
                // round-trips losslessly; the non-adjacent `(int)[]`
                // arm just has the bare `LBRACK_TOK`.
                if matches!(
                    self.peek(),
                    Some((
                        Ok(FilteredToken::Virtual(Virtual::HighPrecedenceBrackApp)),
                        _
                    )),
                ) {
                    self.bump_into(SyntaxKind::ERROR);
                }
                self.bump_into(SyntaxKind::LBRACK_TOK);
                // Gate the rank-commas and the closing `]` on the raw
                // cursor (not filtered `peek`): LexFilter swallows
                // `RPAREN`, so on malformed input like `(x : int[), y)`
                // the filtered next token after `LBRACK_TOK` is the
                // outer tuple's `,` / `)`, which the loop would
                // otherwise absorb into the array suffix and drag the
                // real closing paren in as `ERROR`. Mirrors the
                // raw-stream guard `parse_atomic_type`'s DOT loop uses
                // for the same swallowed-`)` reason.
                while self
                    .next_non_trivia_raw_at_pos()
                    .is_some_and(|t| matches!(t, Token::Comma))
                {
                    self.bump_into(SyntaxKind::COMMA_TOK);
                }
                if self
                    .next_non_trivia_raw_at_pos()
                    .is_some_and(|t| matches!(t, Token::RBrack))
                {
                    self.bump_into(SyntaxKind::RBRACK_TOK);
                }
                self.builder.finish_node();
            } else if next.is_some_and(raw_starts_postfix_app_head)
                && matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Raw(t)), _)) if raw_starts_postfix_app_head(t)
                )
            {
                // The raw-stream gate above keeps a LexFilter-swallowed `)`
                // between the LHS and the next filtered token from being
                // crossed (pinned by
                // `app_type_post_head_lookahead_does_not_cross_swallowed_rparen`),
                // but the raw lookahead also skips *parser-visible* layout
                // virtuals the filtered cursor still sits on — notably
                // `Virtual::RightBlockEnd` (`ORIGHT_BLOCK_END`) closing a `->`
                // one-sided block. In `fun x -> x : int⏎y`, the raw next token
                // is the next line's `y` while the filtered cursor is parked on
                // that virtual; dispatching `parse_app_type_con_power` there
                // trips its `unreachable!`. So mirror the DOT / array-suffix
                // loops and confirm the *filtered* cursor is itself on the
                // postfix head before continuing — any parked virtual ends the
                // type here. The loop-top gate only excludes `BlockSep` /
                // `BlockEnd`, so this is the exhaustive guard for the rest.
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_TYPE));
                // Call `parse_app_type_con_power` (path/typar only) —
                // *not* `parse_atomic_type`. FCS's postfix-app rule
                // (`appTypeWithoutNull appTypeConPower`,
                // `pars.fsy:6378`) only admits `appTypeConPower` on
                // the right; if we used the full `parse_atomic_type`
                // here it'd pick up the prefix-app HPA wrap and let
                // `int Foo<string>` parse as
                // `App(App(Foo, [string]), [int], postfix)` —
                // a shape FCS rejects.
                self.parse_app_type_con_power();
                self.builder.finish_node();
            } else {
                break;
            }
        }
    }

    /// Whether the cursor sits on an `intersectionType` head
    /// (`pars.fsy:6328-6335`, phase 10.10) — the only heads FCS admits before
    /// the `&` of a constraint intersection:
    ///
    /// * a `hashConstraint` (`#…`) — always a head; any prefix-app stays
    ///   *inside* the `#…` (`#Foo<int>` is still a bare hash constraint), so a
    ///   following `<` does not disqualify it; or
    /// * a *bare* `typar` (`'T` / `^T`) — a head only when the typar ident is
    ///   *immediately* followed by `&` (`intersectionType: typar AMP …`). Any
    ///   other continuation makes it not a head, and FCS errors on those: a `<`
    ///   HPA prefix-app (`'T<int> & …`), a `^` measure-power (`'T^2 & …`, which
    ///   FCS reduces to `MeasurePower` then errors on the `&`), or a postfix-app
    ///   (`'T list & …`). Requiring the third significant raw to be `&` rules out
    ///   all of them in one check.
    ///
    /// Classified on the raw stream *before* the head is parsed. For the
    /// variable-length `hashConstraint` head (`#Foo<int>`) the `&` cannot be
    /// found by a fixed lookahead, so the hash arm returns `true` and the caller
    /// confirms with a *post-parse* `&` check (after `parse_atomic_type`
    /// consumes the whole `#…`); a postfix-applied hash head (`#A list &`) then
    /// declines because the `&` no longer follows the bare atom.
    fn at_intersection_head(&self) -> bool {
        match self.nth_significant_raw_at_pos(0) {
            Some(Token::Hash) => true,
            Some(Token::Quote | Token::Op("^")) => {
                matches!(
                    self.nth_significant_raw_at_pos(1),
                    Some(Token::Ident(_) | Token::QuotedIdent(_))
                ) && matches!(self.nth_significant_raw_at_pos(2), Some(Token::Amp))
            }
            _ => false,
        }
    }

    /// FCS's `intersectionType` tail (`pars.fsy:6328-6335`): after the head
    /// `typar` / `hashConstraint` (already parsed under `cp`), the
    /// `& <constraint>` run, retro-wrapping everything as
    /// [`SyntaxKind::INTERSECTION_TYPE`]. Each operand is a `hashConstraint`
    /// (`#T`, flexible — valid) or a plain `atomType` (non-flexible → FCS error
    /// 3572 `parsConstraintIntersectionSyntaxUsedWithNonFlexibleType`, still
    /// parsed). The `&` and operand both gate on the *raw* stream first (the
    /// swallowed-`)` guard the tuple / nullable layers use) before bumping.
    ///
    /// The head node's kind carries FCS's `typar option` discriminant: a
    /// `VAR_TYPE` head is the `Some typar` form; a `HASH_CONSTRAINT_TYPE` head
    /// is the `None` form (where the head hash is the first `types` element).
    /// The facade [`IntersectionType::typar`](crate::syntax::IntersectionType)
    /// recovers it, so the parser stores the head as a plain first child either
    /// way.
    fn parse_intersection_tail(&mut self, cp: rowan::Checkpoint) {
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::INTERSECTION_TYPE));
        self.parse_intersection_constraint_run();
        self.builder.finish_node();
    }

    /// The `(& <flexible-type>)*` run shared by the `intersectionType` tail
    /// (`SynType.Intersection`, `'T & 'U` in type position) and a
    /// `SynTyparDecl`'s intersection constraints (`'t & #seq<int>` in a `<…>`
    /// declaration list, [`Parser::parse_typar_decl`]). Emits each `&` as an
    /// [`SyntaxKind::AMP_TOK`] then the operand type into the *current* node, so
    /// the caller decides the wrapper (`INTERSECTION_TYPE` vs `TYPAR_DECL`).
    ///
    /// Each operand is a `hashConstraint` (`#atomType`, flexible — valid) or a
    /// plain `atomType` (non-flexible → FCS error 3572
    /// `parsConstraintIntersectionSyntaxUsedWithNonFlexibleType`, still parsed).
    /// The `&` and operand both gate on the *raw* stream first (the swallowed-`)`
    /// guard the tuple / nullable layers use), then confirm the filtered cursor
    /// before bumping. A missing operand errors and stops the run.
    pub(super) fn parse_intersection_constraint_run(&mut self) {
        while self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Amp))
        {
            // Confirm the filtered cursor itself is at the `&` (no swallowed
            // `)` / virtual the raw lookahead skipped past) so `bump_into`
            // consumes the right token.
            if !matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Amp)), _))) {
                break;
            }
            self.bump_into(SyntaxKind::AMP_TOK);
            // The operand is an `atomType` (which subsumes `hashConstraint`); a
            // non-`#` operand draws FCS's "non-flexible type" diagnostic but is
            // still parsed into the `types` list. Gate on the raw stream first.
            let operand_is_hash = matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Hash));
            if self.peek_starts_atomic_type() {
                if !operand_is_hash {
                    let span = self
                        .peek()
                        .map(|(_, span)| span.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message: "constraint intersection may only use flexible types, \
                                  e.g. `#IDisposable`"
                            .to_string(),
                        span,
                    });
                }
                self.parse_atomic_type();
            } else {
                let span = self
                    .peek()
                    .map(|(_, span)| span.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected type after `&` in constraint intersection".to_string(),
                    span,
                });
                break;
            }
        }
    }

    /// FCS's `typeArgActual` (`pars.fsy:6664-6675`, phase 10.9) — one actual
    /// type argument inside a `<…>` block. Either a plain `typ`, or the named
    /// static-constant form `typ EQUALS typ` (`SynType.StaticConstantNamed`):
    /// `Foo<N=42>` projects the first `typ` as the name and the second as the
    /// value (both full `SynType`s — `42` becomes a `StaticConstant`, `int`
    /// stays a `LongIdent`). The shared checkpoint retro-wraps the already-parsed
    /// first `typ` once an `=` follows.
    ///
    /// The `=` is detected on the *raw* stream first — a LexFilter-swallowed `)`
    /// between the arg and an outer `=` must not be crossed — then confirmed on
    /// the filtered cursor so `bump_into` consumes the right token. The value
    /// side is a plain `parse_type` (FCS's `typeArgActual` does not nest, so
    /// `N=M=P` is not a chain).
    pub(super) fn parse_type_arg_actual(&mut self) {
        let cp = self.builder.checkpoint();
        self.parse_type();
        if !self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Equals))
        {
            return;
        }
        if !matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Equals)), _))
        ) {
            return;
        }
        self.builder.start_node_at(
            cp,
            FSharpLang::kind_to_raw(SyntaxKind::STATIC_CONST_NAMED_TYPE),
        );
        self.bump_into(SyntaxKind::EQUALS_TOK);
        self.parse_type();
        self.builder.finish_node();
    }

    /// FCS's `atomTypeOrAnonRecdType: atomType | anonRecdType`
    /// (`pars.fsy:6520`) — an atomic type or an anonymous record type. The
    /// anon-recd dispatch sits here — *not* inside [`Parser::parse_atomic_type`]
    /// — so the hash branch's recursive `parse_atomic_type` call (FCS's strict
    /// `atomType`) cannot accept `#{| F : int |}`, matching FCS's rejection of
    /// that surface ("Unexpected symbol `{|`" at the inner hash recovery point).
    ///
    /// This is the type level reached by `appType`'s head
    /// (`atomTypeOrAnonRecdType (…)*`, `pars.fsy:6378`) and by the IsInst
    /// pattern (`COLON_QMARK atomTypeOrAnonRecdType`, `pars.fsy:3729`), so both
    /// share this one method.
    pub(super) fn parse_atom_type_or_anon_recd_type(&mut self) {
        if self.peek_starts_anon_recd_type() {
            self.parse_anon_recd_type();
        } else {
            // A `struct (` struct-tuple type is an `atomType`, so it is dispatched
            // inside `parse_atomic_type` (which `#` also recurses into).
            self.parse_atomic_type();
        }
    }

    /// Atomic type — FCS's `atomType` (`pars.fsy:6534-6549`). Covers:
    ///
    /// * `Ident (DOT Ident)*` → `LONG_IDENT_TYPE > LONG_IDENT > [IDENT_TOK,
    ///   DOT_TOK, IDENT_TOK, …]` (FCS's `SynType.LongIdent(SynLongIdent)`).
    /// * `'a` / `^T`          → `VAR_TYPE > [(QUOTE_TOK | HAT_TOK),
    ///   IDENT_TOK]` (FCS's `SynType.Var(SynTypar, range)`).
    /// * `_`                  → `ANON_TYPE > [UNDERSCORE_TOK]`
    ///   (FCS's `SynType.Anon(range)`).
    /// * `( typ )`            → `PAREN_TYPE > [LPAREN_TOK, <typ>,
    ///   RPAREN_TOK]` (FCS's `SynType.Paren(innerType, range)`).
    /// * `#T`                 → `HASH_CONSTRAINT_TYPE > [HASH_TOK,
    ///   <inner-atomic-type>]` (FCS's `SynType.HashConstraint(inner,
    ///   range)`, `pars.fsy:2609-2611`). Inner is a recursive
    ///   `parse_atomic_type` so `##int`, `#Foo<int>`,
    ///   `#(int -> int)`, `#'T`, etc. all collapse here.
    /// * `Foo<arg, …>`        → `APP_TYPE` prefix form
    ///   (`pars.fsy:6618`). LexFilter inserts a zero-width
    ///   [`Virtual::HighPrecedenceTyApp`] between the head ident and
    ///   the typar-bracket `<` (and promotes the `Less`/`Greater` bool
    ///   payload to flag them as typar brackets). When that virtual
    ///   follows the head atomic, retroactively wrap via the same
    ///   `start_node_at(cp, APP_TYPE)` pattern as the postfix loop,
    ///   shape
    ///   `[<head>, ERROR(HPA, zero-width), LESS_TOK, <arg>, (COMMA_TOK,
    ///   <arg>)*, GREATER_TOK]`. `>>` / `>=` at the close are pre-split
    ///   by LexFilter's `smash_typar_token`, so the closer is always a
    ///   bare `Greater`.
    ///
    /// Layer placement matches FCS exactly: `atomType` includes both
    /// `hashConstraint` and the prefix-app (`path HPA LESS … GREATER`)
    /// surface form, so the HPA wrap lives here rather than at the
    /// postfix-loop layer above. This is what makes `#Foo<int>` produce
    /// `HashConstraint(App(Foo, [int]))` instead of
    /// `App(HashConstraint(Foo), [int])` — the hash branch recurses
    /// into `parse_atomic_type`, which then runs the HPA wrap on the
    /// inner head.
    ///
    /// `RPAREN` is swallowed by LexFilter in the same way as for paren-
    /// expressions, so [`Parser::bump_swallowed_rparen`] handles the close.
    pub(super) fn parse_atomic_type(&mut self) {
        // `#T` flexible-type constraint — `pars.fsy:2609-2611`
        // (`HASH atomType`). Recurse into `parse_atomic_type` to cover
        // the nested-hash / prefix-app-inside-hash / paren-inside-hash
        // cases naturally; no HPA wrap belongs at the hash level
        // because LexFilter never emits an HPA virtual immediately
        // after a `Hash` raw.
        //
        // Gate the recursion on the next non-trivia *raw* token being
        // an atomic-type starter — for incomplete input like `(x : #)`
        // the filtered peek lands on a token past the (swallowed)
        // closing paren, and recursing unconditionally would either
        // panic at the `unreachable!` arm below or have the inner
        // call consume something past the paren-type boundary. The
        // recovery emits a `ParseError` and leaves the hash node with
        // no inner-type child (`HashConstraintType::inner` becomes
        // `None`).
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Hash)), _))) {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::HASH_CONSTRAINT_TYPE));
            self.bump_into(SyntaxKind::HASH_TOK);
            // `HASH atomType` — share the folded-literal-aware atomType gate so
            // `#-1` parses as `HashConstraint(StaticConstant -1)` (FCS) rather
            // than rejecting the folded literal whose raw cursor is `Op("-")`.
            if self.peek_starts_atomic_type() {
                // Depth-guarded: nested `#` flexible-type constraints
                // (`#####int`) recurse here *below* `parse_type`'s guard, so
                // count each level or the chain overflows despite that guard.
                self.with_depth(|p| p.parse_atomic_type());
            } else {
                let span = self
                    .peek()
                    .map(|(_, span)| span.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected atomic type after `#`".to_string(),
                    span,
                });
            }
            self.builder.finish_node();
            return;
        }

        let cp = self.builder.checkpoint();
        // Whether the head atomic is one FCS admits as the LHS of a
        // dot-chain (`atomType DOT path`, `pars.fsy:6600-6605`). FCS's
        // LR tables empirically accept Paren / Anon (`_`) / HPA-wrapped
        // prefix-App as LHS, but reject bare typar (`'T.Foo`) and
        // already-handle the `Foo.Bar.Baz` long-ident inside
        // [`Parser::parse_app_type_con_power`]'s eager DOT loop. The
        // `#int.Foo` shape parses as `HashConstraint(int.Foo)` in FCS,
        // i.e. the dot extends the inner long-ident rather than
        // wrapping the hash — so HashConstraint LHS is also disallowed
        // here. Tracked as a per-arm boolean so the HPA wrap below can
        // promote a previously-rejected head (e.g. `'T` → `'T<int>`)
        // into an acceptable one without changing this method's match
        // shape.
        let mut head_can_chain;
        // Whether the head is an `appTypeCon` (a path or typar) — the *only*
        // head FCS admits the `path HPA LESS …` prefix-app wrap on
        // (`pars.fsy:6618`). LexFilter emits the HPA virtual after *any*
        // adjacent-`<` head, **including a numeric literal** (`42<int>`), so the
        // wrap below must be gated on this rather than on the mere presence of
        // the HPA — otherwise a static-constant / `_` / paren head would wrap as
        // `App(StaticConstant 42, [int])` where FCS yields `StaticConstant 42` +
        // an "unexpected type application" error.
        let mut head_is_app_con = false;
        // `struct ( T1 * T2 [* …] )` — a struct-tuple type. FCS places `STRUCT
        // LPAREN …` under `atomType` (not the wider `atomTypeOrAnonRecdType`, so
        // unlike the anon record `struct {|` it is reachable through `#` —
        // `#struct (int * int)`), and admits it as a dot-chain LHS like a `Paren`
        // (`struct (int * int).Nested` → `LongIdentApp`), so it is parsed here at
        // the head, *under* the `cp` checkpoint, with `head_can_chain = true` so
        // the dot-chain loop below can wrap a trailing `.path`. It is not an
        // `appTypeCon`, so `head_is_app_con` stays `false` (no `<…>` type-arg
        // wrap). Depth-guarded like the `#` recursion above: a nested
        // `struct (struct (…) * int)` re-enters here below `parse_type`'s own
        // guard, so each level must be counted or a deep nest overflows the stack.
        if self.peek_starts_struct_tuple_type() {
            self.with_depth(|p| p.parse_struct_tuple_type());
            head_can_chain = true;
        } else {
            match self.peek().cloned() {
                Some((Ok(FilteredToken::Raw(Token::Underscore)), _)) => {
                    self.parse_anon_type();
                    head_can_chain = true;
                }
                Some((
                    Ok(FilteredToken::Raw(
                        Token::Ident(_)
                        | Token::QuotedIdent(_)
                        | Token::Quote
                        | Token::Op("^")
                        // `global.Path` — the global-namespace root as a type
                        // path head (FCS's `GLOBAL DOT …`). FCS treats the
                        // `global` keyword as an identifier heading a
                        // `SynType.LongIdent`, so it enters `parse_app_type_con_power`
                        // like an ordinary path head and is emitted as an
                        // `IDENT_TOK` there.
                        | Token::Global,
                    )),
                    _,
                )) => {
                    // Path / typar head — `appTypeConPower`'s inner
                    // `appTypeCon` (`pars.fsy:6337-6342`). Extracted so the
                    // postfix-app right-hand layer in [`Parser::parse_app_type`]
                    // can call it without also picking up the HPA prefix-app
                    // wrap that follows below.
                    let consumed_power = self.parse_app_type_con_power();
                    // Ident: `parse_app_type_con_power` already greedily
                    // walked `DOT IDENT` pairs into the LONG_IDENT, so a
                    // further dot-chain would either redundantly steal
                    // those segments or (after FCS rejects `'T.Foo`-style
                    // shapes) violate the LR oracle. Typar: FCS rejects
                    // bare `'T.Foo`. Either way, disable until the HPA
                    // wrap below upgrades the head to APP_TYPE.
                    head_can_chain = false;
                    // Eligible for the prefix-app wrap only as a *plain*
                    // `appTypeCon`: FCS's `appTypeCon typeArgsNoHpaDeprecated`
                    // (`pars.fsy:6596`) does not extend to `appTypeConPower`, so a
                    // consumed measure-power tail (`Foo^2`) makes the head an
                    // `appTypeConPower` and the following `<…>` is rejected — the
                    // marker / `<` is left for the enclosing context's recovery
                    // (FCS's "unexpected type application" / "unexpected `<`").
                    head_is_app_con = !consumed_power;
                }
                Some((Ok(FilteredToken::Raw(Token::LParen)), _)) => {
                    self.builder
                        .start_node(FSharpLang::kind_to_raw(SyntaxKind::PAREN_TYPE));
                    self.bump_into(SyntaxKind::LPAREN_TOK);
                    // Check the *raw* stream before draining: LexFilter has
                    // swallowed the matching `)`, so the filtered peek can
                    // land on a token *past* the closing paren of this
                    // paren-type. For an empty `()` (or `(  )`) inside a
                    // larger expression like `(x : ()) y`, draining first
                    // would consume the inner+outer `)` as `ERROR` and then
                    // accept `y` as the body. Gate on the raw stream so the
                    // recovery error fires at the in-paren boundary without
                    // stealing tokens past it (mirrors `parse_type`). Uses the
                    // `typ`-level [`Parser::peek_starts_type`] (not the atomic
                    // gate) so a parenthesised leading-`/` measure `(/s)` reaches
                    // the `parse_type` call below, matching FCS.
                    if self.peek_starts_type() {
                        if let Some((_, next_span)) = self.peek() {
                            let start = next_span.start;
                            self.drain_raw_up_to(start);
                        }
                        self.parse_type();
                    } else {
                        let span = self
                            .peek()
                            .map(|(_, span)| span.clone())
                            .unwrap_or_else(|| self.source.len()..self.source.len());
                        self.errors.push(ParseError {
                            message: "expected type inside parentheses".to_string(),
                            span,
                        });
                    }
                    self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
                    self.builder.finish_node();
                    head_can_chain = true;
                }
                // Type-provider static-constant heads — FCS's `atomicType`
                // (`pars.fsy:6575-6589`, phase 10.9). These typecheck only as
                // type-provider static arguments, but the *grammar* admits them
                // wherever an `atomType` is expected, so they sit at this layer
                // (not gated to the `<…>` arg loop). None can be a dot-chain LHS
                // (FCS reduces past `atomType` before a `.` shift could fire on a
                // literal / `null` / `const E`), so each leaves `head_can_chain`
                // off.
                Some((Ok(FilteredToken::Raw(Token::Null)), _)) => {
                    self.builder
                        .start_node(FSharpLang::kind_to_raw(SyntaxKind::STATIC_CONST_NULL_TYPE));
                    self.bump_into(SyntaxKind::NULL_TOK);
                    self.builder.finish_node();
                    head_can_chain = false;
                }
                Some((Ok(FilteredToken::Raw(Token::Const)), _)) => {
                    // `CONST atomicExpr` (`pars.fsy:6583`).
                    self.builder
                        .start_node(FSharpLang::kind_to_raw(SyntaxKind::STATIC_CONST_EXPR_TYPE));
                    self.bump_into(SyntaxKind::CONST_TOK);
                    // Gate the operand on [`Parser::peek_starts_const_arg_expr`] — a
                    // raw-aligned, LParen-safe restriction to exactly what
                    // [`Parser::parse_atomic_expr`] consumes without panicking. This
                    // rejects (a) a swallowed closer (`(x : const) y` — raw cursor
                    // `)` ), (b) a non-atomic starter (`const if …`, a bare
                    // `const -` — would reach `parse_const_payload`'s `unreachable!`
                    // arm), and (c) a malformed paren (`const (>` — would reach the
                    // LParen-dispatch `unreachable!`); a sign-folded `const -1` and
                    // a `const (e)` paren expr are admitted. All rejections error
                    // cleanly, matching FCS.
                    if self.peek_starts_const_arg_expr() {
                        // Drain the trivia between `const` and the expression so it
                        // attaches to the outer `STATIC_CONST_EXPR_TYPE` (matching
                        // `parse_quote_expr`'s post-opener drain) rather than the
                        // inner expr's node.
                        if let Some((_, next_span)) = self.peek() {
                            let start = next_span.start;
                            self.drain_raw_up_to(start);
                        }
                        self.parse_atomic_expr();
                    } else {
                        let span = self
                            .peek()
                            .map(|(_, span)| span.clone())
                            .unwrap_or_else(|| self.source.len()..self.source.len());
                        self.errors.push(ParseError {
                            message: "expected atomic expression after `const`".to_string(),
                            span,
                        });
                    }
                    self.builder.finish_node();
                    head_can_chain = false;
                }
                // A bare literal (`rawConstant` / `TRUE` / `FALSE`) →
                // `StaticConstant`. `LParen` is already claimed by the
                // `PAREN_TYPE` arm above, so the `raw_starts_const_payload`
                // overlap on `(` never reaches here.
                Some((Ok(FilteredToken::Raw(tok)), _)) if raw_starts_const_payload(&tok) => {
                    self.builder
                        .start_node(FSharpLang::kind_to_raw(SyntaxKind::STATIC_CONST_TYPE));
                    self.parse_const_payload();
                    self.builder.finish_node();
                    head_can_chain = false;
                }
                other => {
                    unreachable!("parse_atomic_type called with non-type-starter: {other:?}")
                }
            }
        }

        // Optional prefix-app wrap — `appTypeCon typeArgsNoHpaDeprecated`
        // (`pars.fsy:6596`), `SynType.App`. The type-args block is either
        // `HIGH_PRECEDENCE_TYAPP typeArgsActual` (the adjacent `Foo<int>`
        // form) or a bare `typeArgsActual` (the spaced `Foo < int >` form
        // FCS accepts with warning FS1190); `consume_type_args_no_hpa`
        // handles both. Gated on `head_is_app_con` (path / typar): LexFilter
        // emits the HPA virtual after *any* adjacent-`<` head including a
        // numeric literal (`42<int>`), but FCS only admits `<…>` after an
        // `appTypeCon`, so a static-constant / `_` / paren head must leave
        // the marker unconsumed (it surfaces wherever the caller next
        // examines the cursor, e.g. the typed-paren close, mirroring FCS's
        // "unexpected type application" error). A `'T<int>` typar head *is*
        // an `appTypeCon` and still wraps.
        if head_is_app_con && self.at_type_args_no_hpa() {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_TYPE));
            self.consume_type_args_no_hpa();
            self.builder.finish_node();
            // `Foo<int>` / `Foo < int >` (and `'T<int>` per FCS's permissive
            // HPA acceptance) admit a trailing dot-chain.
            head_can_chain = true;
        }

        // Dot-chain: `atomType DOT path [typeArgsNoHpaDeprecated]`
        // (`pars.fsy:6600-6605`), `SynType.LongIdentApp`. Left-recursive
        // in the grammar, looped here. The checkpoint `cp` was captured
        // at the start of this method and points to the head atomic;
        // each iteration retro-wraps everything captured so far as the
        // root of a new `LONG_IDENT_APP_TYPE`, giving left-associative
        // nesting (`(int).B.C` becomes `LongIdentApp(LongIdentApp(
        // Paren, [B]), [C])`).
        //
        // Gated on `head_can_chain` to match FCS's empirical LR
        // acceptance:
        // * `(int).Foo`, `(int list).Foo`, `_.Foo`, `Foo<int>.Bar`,
        //   `'T<int>.Foo` — *accepted* (Paren / Anon / HPA-wrapped App
        //   heads).
        // * `'T.Foo`, `#int.Foo`, `Foo.Bar.Baz` — *rejected* (bare
        //   typar / hash / already-greedy LongIdent). For the long-
        //   ident case, `parse_app_type_con_power` already absorbed
        //   the dots, so firing this loop would be redundant and
        //   double-eat segments. For typar / hash, FCS's parser tables
        //   reduce past `atomType` before reaching the DOT-shift
        //   state, so emitting a LongIdentApp here would diverge from
        //   `fcs-dump ast`.
        if !head_can_chain {
            return;
        }
        loop {
            // Gate on the *raw* stream for the `.`: an intervening
            // LexFilter-swallowed `)` (e.g. the closing paren of an
            // enclosing `(e : T)`) sits between this atomic and the
            // filtered cursor's next token, and consuming the outer
            // `.member` here would steal members from the enclosing
            // expression-dot-access. Mirrors the swallowed-`)` guard in
            // `parse_app_type_con_power`'s DOT loop.
            if !self
                .next_non_trivia_raw_at_pos()
                .is_some_and(|t| matches!(t, Token::Dot))
            {
                break;
            }
            // Confirm the filtered cursor itself is at the `.` (no
            // intervening filtered virtual): the raw-stream check
            // skipped trivia and virtuals; we also need the filtered
            // peek to actually point at the dot so `bump_into` consumes
            // the right token. A virtual layout boundary between the
            // atomic and the dot means this is a new top-level token
            // (e.g. a member access on a *value* whose type is on the
            // preceding line), not a continuation of the type.
            let Some((Ok(FilteredToken::Raw(Token::Dot)), dot_span)) = self.peek().cloned() else {
                break;
            };
            // The post-dot ident also needs raw-stream gating for the
            // same reason: a swallowed `)` followed by an outer ident
            // would otherwise be misclassified as the path-tail of
            // *this* type's LongIdentApp.
            if !matches!(
                self.next_non_trivia_raw_after(dot_span.end),
                Some(Token::Ident(_) | Token::QuotedIdent(_)),
            ) {
                break;
            }
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_APP_TYPE));
            self.bump_into(SyntaxKind::DOT_TOK);
            // Walk the path: first ident, then greedy `DOT IDENT`
            // pairs. Mirrors `parse_app_type_con_power`'s inner loop,
            // including the trailing-dot recovery.
            //
            // Every bump below is gated on the *filtered* cursor
            // actually pointing at the expected raw token. The raw
            // lookahead helpers (`next_non_trivia_raw_at_pos`) skip
            // layout virtuals, but `bump_into` consumes whatever is at
            // the filtered cursor — so when a `Virtual(BlockSep)` sits
            // between the `.` and the ident (e.g. an anon-record field
            // type wrapping onto a new line at the field's offside
            // column), an unguarded bump would emit that zero-width
            // virtual as a path token and leave the real token behind.
            // A layout boundary inside the path is a hard stop: FCS
            // rejects the surface, and we break with the same
            // trailing-dot recovery rather than crossing it.
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
            if matches!(
                self.peek(),
                Some((
                    Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                    _
                )),
            ) {
                self.bump_into(SyntaxKind::IDENT_TOK);
                while self
                    .next_non_trivia_raw_at_pos()
                    .is_some_and(|t| matches!(t, Token::Dot))
                {
                    // Confirm the filtered cursor is at the dot, not a
                    // layout virtual the raw lookahead skipped past.
                    let Some((Ok(FilteredToken::Raw(Token::Dot)), inner_dot_span)) =
                        self.peek().cloned()
                    else {
                        break;
                    };
                    self.bump_into(SyntaxKind::DOT_TOK);
                    // Gate the next ident on the *raw* stream, exactly
                    // as the entry dot does. LexFilter swallows the
                    // close paren of a typed expression, so for an
                    // incomplete annotation like `(x : (int).Foo.) y`
                    // the filtered cursor exposes the outer `y` while
                    // the raw stream still has the `)` between this dot
                    // and `y`. Accept the ident only when the raw token
                    // after the dot is itself an ident; otherwise this
                    // is a trailing dot and crossing it would steal `y`
                    // and drain the real `)` as ERROR.
                    let raw_next_is_ident = matches!(
                        self.next_non_trivia_raw_after(inner_dot_span.end),
                        Some(Token::Ident(_) | Token::QuotedIdent(_)),
                    );
                    match self.peek().cloned() {
                        Some((
                            Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                            _,
                        )) if raw_next_is_ident => {
                            self.bump_into(SyntaxKind::IDENT_TOK);
                        }
                        _ => {
                            self.errors.push(ParseError {
                                message: "trailing dot in long identifier path".to_string(),
                                span: inner_dot_span,
                            });
                            break;
                        }
                    }
                }
            } else {
                // The entry `.` is followed by a layout boundary (the
                // raw stream has an ident, but the filtered cursor is a
                // virtual) — no path segment is available on this
                // logical line.
                self.errors.push(ParseError {
                    message: "trailing dot in long identifier path".to_string(),
                    span: dot_span,
                });
            }
            self.builder.finish_node(); // close LONG_IDENT

            // Optional `typeArgsNoHpaDeprecated` (`pars.fsy:6603`),
            // `SynType.LongIdentApp` — the same adjacent-or-spaced type-args
            // block as the `appTypeCon` head above. `consume_type_args_no_hpa`
            // owns the HPA / bare-`<` handling and the swallowed-`)`
            // raw-stream guards (a `<` after a LexFilter-swallowed `)`, e.g.
            // `(e : T)` then an outer comparison `< y`, is left alone).
            if self.at_type_args_no_hpa() {
                self.consume_type_args_no_hpa();
            }
            self.builder.finish_node(); // close LONG_IDENT_APP_TYPE
        }
    }

    /// `true` if the cursor opens a `typeArgsNoHpaDeprecated`
    /// (`pars.fsy:6611`) block — the optional `HIGH_PRECEDENCE_TYAPP`
    /// marker plus a `LESS … GREATER` type-argument list — at the current
    /// position. Also gates the `enum<…>` / `delegate<…>` typar-constraint
    /// arms ([`Parser::parse_typar_constraint_after_colon`]). This is either the adjacent form (`Foo<int>`, marked by
    /// the [`Virtual::HighPrecedenceTyApp`] virtual LexFilter inserts
    /// before an adjacent `<`) or the spaced / deprecated form
    /// (`Foo < int >`, a bare `<` FCS accepts as `SynType.App` with
    /// warning FS1190).
    ///
    /// The bare-`<` arm gates on the *raw* stream: a LexFilter-swallowed
    /// `)` between the head and the next filtered `<` (e.g. the closing
    /// paren of an enclosing `(e : T)` followed by an outer comparison
    /// `< y`) leaves the raw cursor at the `)` while `peek()` already
    /// shows the `<`. Requiring a raw `<` too declines that case, so the
    /// `<` is left for the enclosing expression's comparison rather than
    /// drained as a spurious type-arg opener.
    pub(super) fn at_type_args_no_hpa(&self) -> bool {
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)), _)),
        ) {
            return true;
        }
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Less(_))), _)),
        ) && self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Less(_)))
    }

    /// Consume a `typeArgsNoHpaDeprecated` (`pars.fsy:6611`) block as
    /// children of the currently-open node — the optional
    /// `HIGH_PRECEDENCE_TYAPP` marker, then `LESS typeArgsActual GREATER`.
    /// The caller must have confirmed `at_type_args_no_hpa` and opened the
    /// wrapping node (`APP_TYPE` / `LONG_IDENT_APP_TYPE`) first. Both the
    /// adjacent (`Foo<int>`) and spaced / deprecated (`Foo < int >`, FCS
    /// warning FS1190) forms route here and build the same shape, since our
    /// parser has no warning channel.
    pub(super) fn consume_type_args_no_hpa(&mut self) {
        // HPA virtual (adjacent form): consume as a zero-width ERROR
        // (mirrors `HighPrecedenceParenApp`'s treatment in
        // `parse_app_expr`). The spaced form has no marker.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)), _)),
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
        // A lone HPA with no following `<` is degenerate (LexFilter only
        // emits the marker before an adjacent `<`); nothing more to do.
        if !matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Less(_))), _)),
        ) {
            return;
        }
        // The early return above guarantees the cursor is at the `<`.
        let less_span = self
            .peek()
            .map(|(_, span)| span.clone())
            .expect("cursor is at the `<` opener (just matched above)");
        self.bump_into(SyntaxKind::LESS_TOK);
        // Empty type-arg list `Foo< >` — FCS's `typeArgsActual: LESS
        // GREATER` arm (`pars.fsy:6649`) yields zero args with no error.
        // (Adjacent `<>` fuses into the `<>` inequality op, so the empty
        // form only arises spaced.) Skip the arg loop when the close `>`
        // is already next, leaving the GREATER bump below to consume it.
        if !matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Greater(_))), _)),
        ) {
            self.parse_type_arg_actual();
            // Gate the inner `,` bump against a LexFilter-swallowed `)`
            // between the last type arg and an outer separator (e.g.
            // `(x : Foo<Bar) , y`): the swallowed `)` leaves the raw
            // cursor at the `RParen` while `peek()` already shows the
            // outer `,`. Without the guard the bump would drain the `)`
            // as ERROR and steal the outer token. `,` never fuses with an
            // adjacent op, so a raw-token *type* check suffices: a real
            // separator leaves `Comma` at the raw cursor; a swallowed `)`
            // leaves `RParen`.
            while self
                .next_non_trivia_raw_at_pos()
                .is_some_and(|t| matches!(t, Token::Comma))
                && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Comma)), _)),)
            {
                self.bump_into(SyntaxKind::COMMA_TOK);
                self.parse_type_arg_actual();
            }
        }
        // The closing `>` *can* fuse with a following op char (`>.`, `>>`,
        // `>=`, …) into a single raw `Op`, which LexFilter then splits into
        // a `Greater` filtered piece — so a raw-token *type* check would
        // reject the legitimate close (`Foo<string>.Bar` fuses `>.`).
        // Confirm span alignment instead: the filtered `>` must start
        // *within* the raw token at the cursor. A swallowed `)` sits before
        // the outer `>`, so its span ends before the `>` start and the
        // alignment check fails.
        let closed = if let Some((Ok(FilteredToken::Raw(Token::Greater(_))), greater_span)) =
            self.peek().cloned()
            && self
                .next_non_trivia_raw_at_pos_with_span()
                .is_some_and(|(_, raw_span)| {
                    raw_span.start <= greater_span.start && greater_span.start < raw_span.end
                }) {
            self.bump_into(SyntaxKind::GREATER_TOK);
            true
        } else {
            false
        };
        if !closed {
            // Unterminated type-arg list — no closing `>` was consumed. FCS
            // shifts the `<` as a type-arg opener in *type* position (after
            // an `appTypeCon`) for both the adjacent-without-marker and
            // spaced forms, then errors when the `>` is missing
            // (`parsExpectedTypeArgs` / FS1241), e.g. `a :?> b < c`,
            // `a :?> b<c`, or an unclosed `(x : (int).Foo<Bar) > y` whose
            // `>` belongs to the enclosing comparison. We mirror that with a
            // diagnostic but drain *no* token: the leftover `>` / `,` / `)`
            // (an outer comparison, tuple separator, or swallowed paren)
            // must stay for the enclosing context's recovery — the
            // span-alignment guard above already declined to bump it.
            self.errors.push(ParseError {
                message: "unterminated type argument list: expected `>`".to_string(),
                span: less_span,
            });
        }
    }

    /// `true` if the cursor is positioned at the start of an
    /// anon-record type — `{|` (reference variant) or `struct` whose
    /// next non-trivia raw token is `{|` (struct variant). The `Struct`
    /// arm lookahead is what distinguishes phase 7.9's
    /// `struct {| F : int |}` from later phases' `struct (T * U)` /
    /// struct-defn surfaces; without it the dispatch would commit on
    /// any bare `struct` and then fail to find `{|` inside
    /// [`Parser::parse_anon_recd_type`].
    pub(super) fn peek_starts_anon_recd_type(&self) -> bool {
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::LBraceBar)), _)) => true,
            Some((Ok(FilteredToken::Raw(Token::Struct)), span)) => self
                .next_non_trivia_raw_after(span.end)
                .is_some_and(|t| matches!(t, Token::LBraceBar)),
            _ => false,
        }
    }

    /// `true` if the cursor is positioned at the start of a struct-tuple type —
    /// `struct` whose next non-trivia raw token is `(` ([`Token::LParen`]). The
    /// sibling of [`Self::peek_starts_anon_recd_type`] (`struct {|`): the
    /// lookahead distinguishes `struct (T * U)` from the anon-record
    /// `struct {| … |}` and from a struct type-definition. Consulted at the
    /// `atomType` dispatch and the type-start gates, so a leading `struct (`
    /// reaches [`Self::parse_struct_tuple_type`].
    ///
    /// Gated on *both* streams (mirroring [`Self::peek_starts_atomic_type`] and
    /// the rest of the type parser): the **filtered** cursor must be `struct`
    /// (rejecting a layout virtual) *and* the next non-trivia **raw** token must
    /// be `struct` too — so a LexFilter-swallowed `)` between the cursor and a
    /// following `struct (` (`(x : int *) struct (…)`) surfaces as a raw `RParen`
    /// and blocks the match, keeping the tuple/atom recovery from consuming the
    /// outer `struct` past the closer.
    pub(super) fn peek_starts_struct_tuple_type(&self) -> bool {
        let Some((Ok(FilteredToken::Raw(Token::Struct)), span)) = self.peek() else {
            return false;
        };
        matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Struct))
            && matches!(
                self.next_non_trivia_raw_after(span.end),
                Some(Token::LParen)
            )
    }

    /// Anon-record type — FCS's
    /// `anonRecdType: [STRUCT] LBRACE_BAR recdFieldDeclList bar_rbrace`
    /// (`pars.fsy:2510-2522`), projected to
    /// `SynType.AnonRecd(isStruct, fields, range)`
    /// (`SyntaxTree.fsi:500`). Shape
    /// `ANON_RECD_TYPE > [STRUCT_TOK?, LBRACE_BAR_TOK,
    /// (ANON_RECD_TYPE_FIELD (SEMI_TOK ANON_RECD_TYPE_FIELD)*)?,
    /// BAR_RBRACE_TOK]`.
    ///
    /// Each field is shape
    /// `ANON_RECD_TYPE_FIELD > [IDENT_TOK, COLON_TOK, <typ>]`. FCS's
    /// `recdFieldDecl` arm accepts attributes / mutable / access
    /// (`pars.fsy:2978-2980`), but the AnonRecd post-processor
    /// (`pars.fsy:6526-6529`) errors on those — phase 7.9 admits only
    /// the minimal `ident COLON typ` form to match the *accepted*
    /// language without reproducing the FCS quirk where invalid
    /// surfaces are accepted by the grammar and then rejected by a
    /// post-pass.
    ///
    /// Trailing `;` after the last field is tolerated (mirrors FCS's
    /// `recdFieldDeclList: recdFieldDecl (seps recdFieldDecl)* seps?`,
    /// `pars.fsy:2522`). An empty body `{| |}` is *not* in the
    /// grammar — `recdFieldDeclList` requires at least one
    /// `recdFieldDecl` — so this method emits a `ParseError` if the
    /// first field is missing, but still consumes the closing `|}` for
    /// the lossless invariant.
    pub(super) fn parse_anon_recd_type(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ANON_RECD_TYPE));
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Struct)), _)),
        ) {
            self.bump_into(SyntaxKind::STRUCT_TOK);
        }
        // The `peek_starts_anon_recd_type` gate guarantees `{|` is next
        // (after the optional `struct`); the arm is `unreachable!` to
        // surface caller bugs loudly.
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::LBraceBar)), _)) => {
                self.bump_into(SyntaxKind::LBRACE_BAR_TOK);
            }
            other => {
                unreachable!(
                    "parse_anon_recd_type called without `{{|` after optional `struct`: {other:?}"
                )
            }
        }

        // First field — error if missing. The `BarRBrace` check lets
        // `{| |}` emit a single "expected field" error without
        // stealing tokens past the close.
        let at_close = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::BarRBrace)), _)),
        );
        if at_close {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected at least one field in anon-record type".to_string(),
                span,
            });
        } else {
            self.parse_anon_recd_type_field();
            // Subsequent fields after one `seps` group. FCS's `seps`
            // (`pars.fsy:2522`) is a *single* group — `;`, `OBLOCKSEP`,
            // `SEMICOLON OBLOCKSEP`, or the reverse `OBLOCKSEP SEMICOLON`
            // (`{| F : int\n   ; G : string |}`) — so a repeated separator
            // (`{| F : int; ; G : int |}`) is a parse error; consuming exactly
            // one group per gap (via `consume_one_seps_group`) leaves any extra
            // to trip the field parser's recovery, matching FCS. A trailing
            // group before `|}` is tolerated. The `|}` closer is a real filtered
            // token (unlike the swallowed record `}`), so `at_close` is a plain
            // peek. A *column-aligned* offside field is separated by an
            // `OBLOCKSEP` (`{| F : int⏎ G : int |}`); a *misaligned*
            // continuation emits no separator and stays an error, both matching
            // FCS.
            let at_close = |p: &Self| {
                matches!(
                    p.peek(),
                    Some((Ok(FilteredToken::Raw(Token::BarRBrace)), _))
                )
            };
            while !at_close(self) && self.consume_one_seps_group(at_close) {
                if at_close(self) {
                    break;
                }
                self.parse_anon_recd_type_field();
            }
        }

        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::BarRBrace)), _)),
        ) {
            self.bump_into(SyntaxKind::BAR_RBRACE_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `|}` to close anon-record type".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// Single field of an [`SyntaxKind::ANON_RECD_TYPE`]. FCS's
    /// projection (`pars.fsy:6526-6529`) is the strict `IDENT_TOK
    /// COLON_TOK <typ>` form — attributes / mutable / access are
    /// rejected by FCS's AnonRecd post-pass and we never construct
    /// them here in the first place.
    ///
    /// Recovery: the gate is the colon position. If the ident is
    /// missing we still emit the field node and push a parse error so
    /// the outer loop can keep going on the `;` separators.
    pub(super) fn parse_anon_recd_type_field(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ANON_RECD_TYPE_FIELD));
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                _,
            )),
        ) {
            self.bump_into(SyntaxKind::IDENT_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected field name in anon-record type".to_string(),
                span,
            });
        }
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _)),) {
            self.bump_into(SyntaxKind::COLON_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `:` in anon-record type field".to_string(),
                span,
            });
        }
        // Field type is a full `typ` so `{| F : int -> int |}` parses
        // as `Fun(int, int)` inside the field (matching FCS — the
        // field-decl grammar uses unrestricted `typ`).
        self.parse_type();
        self.builder.finish_node();
    }

    /// FCS's `appTypeConPower` (`pars.fsy:6344-6355`) — a path / typar
    /// head, with no HPA prefix-app wrap and no hash / paren /
    /// underscore. This is what `appTypeWithoutNull` accepts on the
    /// right-hand side of `appTypeWithoutNull appTypeConPower`
    /// (`pars.fsy:6378`), and is strictly *narrower* than
    /// [`Parser::parse_atomic_type`] (FCS's `atomType`,
    /// `pars.fsy:6534-6549`).
    ///
    /// Layered out separately so the postfix-app loop in
    /// [`Parser::parse_app_type`] can call this directly without
    /// also picking up the prefix-app HPA wrap that lives in
    /// `parse_atomic_type`. Without this split, `int Foo<string>`
    /// would parse as `App(App(Foo, [string]), [int], postfix)` — a
    /// shape FCS rejects with a parse error, because at the postfix
    /// layer the right-hand head can only be a bare path or typar.
    ///
    /// Caller is expected to gate on
    /// [`raw_starts_postfix_app_head`] (or the matching arm in
    /// `parse_atomic_type`'s match); the `other` arm is `unreachable!`
    /// to surface caller bugs loudly.
    ///
    /// Phase 10.8 wires the measure-power tail
    /// (`appTypeCon INFIX_AT_HAT_OP atomicRationalConstant`,
    /// `pars.fsy:6344`): after the `appTypeCon` head is parsed, a trailing
    /// `^` / `^-` operator (the only two `INFIX_AT_HAT_OP` spellings FCS
    /// admits here) retro-wraps the head as a [`SyntaxKind::MEASURE_POWER_TYPE`]
    /// — `[<base>, MEASURE_POWER_OP_TOK, <rational-const>]` — so
    /// `(x : float<m^2>)` produces `App(float, [MeasurePower(m, Integer 2)])`.
    /// The base may be a path (`m`) or a typar (`'a`/`^a`); the rational
    /// exponent is parsed by [`Parser::parse_atomic_rational_const`].
    ///
    /// Returns `true` iff a measure-power tail was consumed (so the head is an
    /// `appTypeConPower`, not a plain `appTypeCon`); see
    /// [`Parser::try_parse_measure_power_tail`].
    pub(super) fn parse_app_type_con_power(&mut self) -> bool {
        // Capture before the head so a trailing `^` / `^-` can retro-wrap the
        // head as the base of a `MEASURE_POWER_TYPE`.
        let cp = self.builder.checkpoint();
        match self.peek().cloned() {
            // `Token::Global` heads a `global.Path` type — FCS spells the
            // `global` keyword as an identifier (`idText` = `` `global` ``)
            // heading a `SynType.LongIdent`, so it is bumped as an `IDENT_TOK`
            // segment exactly like an ordinary path head. A bare `global` is a
            // one-segment `LONG_IDENT_TYPE` (the `while` loop simply doesn't
            // fire), matching FCS's single-segment `SynType.LongIdent`.
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_) | Token::Global)),
                _,
            )) => {
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_TYPE));
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
                self.bump_into(SyntaxKind::IDENT_TOK);
                // Each iteration consumes `DOT IDENT`. A trailing dot is
                // a parse error — mirrors `parse_ident_expr` (phase 2).
                //
                // Gate the dot continuation on the next non-trivia *raw*
                // token being `Dot`, not just the filtered peek: an
                // intervening LexFilter-swallowed `)` (e.g. the closing
                // paren of an enclosing `(e : T)`) would otherwise let
                // the loop absorb the outer `.member` into the type's
                // long-ident path and drag the real `)` in as `ERROR`.
                while self
                    .next_non_trivia_raw_at_pos()
                    .is_some_and(|t| matches!(t, Token::Dot))
                {
                    // Confirm the *filtered* cursor is at the dot, not a
                    // layout virtual the raw lookahead skipped past: a
                    // `Virtual(BlockSep)` sits between the path segment
                    // and the `.` when the long-ident wraps onto a new
                    // line at an enclosing block's offside column (e.g.
                    // an anon-record field `{| F : Foo` newline `.Bar`).
                    // Bumping that virtual would emit a zero-width
                    // `DOT_TOK`; instead the layout boundary ends the
                    // path here.
                    let Some((Ok(FilteredToken::Raw(Token::Dot)), dot_span)) = self.peek().cloned()
                    else {
                        break;
                    };
                    self.bump_into(SyntaxKind::DOT_TOK);
                    // Gate the next ident on the *raw* stream: LexFilter
                    // swallows the close paren of a typed expression, so
                    // for an incomplete annotation like `(x : Foo.) y`
                    // the filtered cursor exposes the outer `y` while
                    // the raw stream still has the `)` between this dot
                    // and `y`. Accept only when the raw token after the
                    // dot is itself an ident; otherwise this is a
                    // trailing dot and crossing it would steal `y` and
                    // drain the real `)` as ERROR.
                    let raw_next_is_ident = matches!(
                        self.next_non_trivia_raw_after(dot_span.end),
                        Some(Token::Ident(_) | Token::QuotedIdent(_)),
                    );
                    match self.peek().cloned() {
                        Some((
                            Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                            _,
                        )) if raw_next_is_ident => {
                            self.bump_into(SyntaxKind::IDENT_TOK);
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
                self.builder.finish_node();
                self.builder.finish_node();
            }
            Some((Ok(FilteredToken::Raw(Token::Quote)), _)) => {
                self.parse_var_type(SyntaxKind::QUOTE_TOK);
            }
            Some((Ok(FilteredToken::Raw(Token::Op("^"))), _)) => {
                self.parse_var_type(SyntaxKind::HAT_TOK);
            }
            other => {
                unreachable!(
                    "parse_app_type_con_power called with non-path/typar starter: {other:?}"
                )
            }
        }

        // Measure-power tail on a path / typar head. The general detection
        // lives in `try_parse_measure_power_tail`; it is also called on the
        // *head* atom in `parse_app_type` so a parenthesised / anon-record /
        // prefix-app base (`(m)^2`, `Foo<int>^2`) — which never reaches this
        // method — is wrapped too. Calling it here additionally covers the
        // postfix-loop right-hand factor (`kg m^2`), which `parse_app_type`'s
        // head call does not see.
        self.try_parse_measure_power_tail(cp)
    }

    /// FCS's `powerType: atomTypeOrAnonRecdType INFIX_AT_HAT_OP
    /// atomicRationalConstant` tail (the unit-of-measure power, phase 10.8). If
    /// the type just parsed (rooted at `cp`) is followed by a `^` / `^-`
    /// operator, retro-wrap `[cp .. )` as a [`SyntaxKind::MEASURE_POWER_TYPE`]
    /// — `[<base>, MEASURE_POWER_OP_TOK, <rational-const>]` — and parse the
    /// exponent. FCS admits only the `^` / `^-` spellings of the
    /// `INFIX_AT_HAT_OP`; any other op leaves the type un-wrapped (it surfaces
    /// wherever the postfix / arg layer next examines the cursor).
    ///
    /// Gate on the *raw* stream first — a LexFilter-swallowed `)` between the
    /// base and the next filtered token must not let an outer `^` be mistaken
    /// for a measure operator — then confirm the filtered cursor is itself at
    /// that op so `bump_into` consumes the right token. A no-op when no
    /// measure operator follows, so callers can apply it unconditionally after
    /// any base type.
    ///
    /// Returns `true` iff a power tail was consumed (the base was wrapped as a
    /// `MEASURE_POWER_TYPE`). Callers that distinguish `appTypeCon` from
    /// `appTypeConPower` — only the former admits a `typeArgsNoHpaDeprecated`
    /// wrap (`pars.fsy:6596`) — use this to suppress the type-arg block after
    /// a power.
    pub(super) fn try_parse_measure_power_tail(&mut self, cp: rowan::Checkpoint) -> bool {
        if !self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Op("^" | "^-")))
        {
            return false;
        }
        let Some((Ok(FilteredToken::Raw(Token::Op("^" | "^-"))), _)) = self.peek() else {
            return false;
        };
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEASURE_POWER_TYPE));
        self.bump_into(SyntaxKind::MEASURE_POWER_OP_TOK);
        self.parse_atomic_rational_const();
        self.builder.finish_node();
        true
    }

    /// `atomicRationalConstant` (`pars.fsy:3511-3515`) — the measure-power
    /// exponent after a *bare* `^` operator. An optional standalone prefix
    /// `-` (FCS's `MINUS`) wraps the rest in a
    /// [`SyntaxKind::RATIONAL_CONST_NEGATE`]; the body is an
    /// `atomicUnsignedRationalConstant` (a bare `INT32` →
    /// [`SyntaxKind::RATIONAL_CONST_INTEGER`], or a parenthesised
    /// `( rationalConstant )` → [`SyntaxKind::RATIONAL_CONST_PAREN`]).
    ///
    /// Only a *non-adjacent* `-` reaches this `MINUS` arm: an adjacent `-2`
    /// is folded into a single signed literal by [`super::sign_fold`] (since
    /// the `^` operator is not an atomic-expr-end), so `m^ -2` arrives as a
    /// lone `Integer(-2)`, while the spaced `m^(- 2)` keeps its `-`.
    pub(super) fn parse_atomic_rational_const(&mut self) {
        if self.at_rational_minus() {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::RATIONAL_CONST_NEGATE));
            self.bump_into(SyntaxKind::MINUS_TOK);
            self.parse_atomic_unsigned_rational_const();
            self.builder.finish_node();
        } else {
            self.parse_atomic_unsigned_rational_const();
        }
    }

    /// `atomicUnsignedRationalConstant` (`pars.fsy:3505-3509`): a bare
    /// `INT32` → [`SyntaxKind::RATIONAL_CONST_INTEGER`], or a parenthesised
    /// `( rationalConstant )` → [`SyntaxKind::RATIONAL_CONST_PAREN`] (the only
    /// place a fraction `1/2` can appear). The `)` is consumed via
    /// [`Parser::bump_swallowed_rparen`] — LexFilter swallows the closer in
    /// paren position, exactly as the paren-type arm of
    /// [`Parser::parse_atomic_type`] relies on. Both dispatch arms are
    /// raw-gated ([`Self::at_rational_lparen`] / [`Self::at_int32_exponent`])
    /// so a missing exponent before a swallowed `)` (`m^) …`) declines here
    /// instead of crossing the closer to grab an outer token.
    fn parse_atomic_unsigned_rational_const(&mut self) {
        if self.at_rational_lparen() {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::RATIONAL_CONST_PAREN));
            self.bump_into(SyntaxKind::LPAREN_TOK);
            self.parse_rational_const();
            self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
            self.builder.finish_node();
        } else if self.at_int32_exponent() {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::RATIONAL_CONST_INTEGER));
            self.bump_rational_int();
            self.builder.finish_node();
        } else {
            self.push_rational_const_error();
        }
    }

    /// `rationalConstant` (`pars.fsy:3484-3494`) — the contents of a
    /// parenthesised measure exponent, where the divisor `/` is allowed. An
    /// optional standalone prefix `-` wraps the rest in a
    /// [`SyntaxKind::RATIONAL_CONST_NEGATE`]; the body is an `INT32`
    /// optionally followed by `/ INT32` → [`SyntaxKind::RATIONAL_CONST_RATIONAL`]
    /// (numerator `/` denominator), else a lone
    /// [`SyntaxKind::RATIONAL_CONST_INTEGER`].
    pub(super) fn parse_rational_const(&mut self) {
        if self.at_rational_minus() {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::RATIONAL_CONST_NEGATE));
            self.bump_into(SyntaxKind::MINUS_TOK);
            self.parse_rational_const_body();
            self.builder.finish_node();
        } else {
            self.parse_rational_const_body();
        }
    }

    /// `true` iff a standalone prefix `-` (a rational `Negate`) sits at *both*
    /// the raw and filtered cursors. The raw check is the swallowed-`)` guard
    /// pervasive in this file: LexFilter removes the `)` that closes the
    /// exponent / annotation from the filtered stream, so a filtered-only peek
    /// could already point at an *outer* `-`/`-N` (`m^) -1`) and steal it as
    /// the exponent, draining the real `)` as `ERROR`. (An adjacent `-N` is a
    /// folded signed `INT32`, handled by [`Self::at_int32_exponent`]; this
    /// catches only the spaced form.)
    fn at_rational_minus(&self) -> bool {
        matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Op("-")))
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Op("-"))), _))
            )
    }

    /// `true` iff a `(` opening a parenthesised rational exponent sits at both
    /// the raw and filtered cursors — raw-gated against a swallowed `)` exactly
    /// like [`Self::at_rational_minus`].
    fn at_rational_lparen(&self) -> bool {
        matches!(self.next_non_trivia_raw_at_pos(), Some(Token::LParen))
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::LParen)), _))
            )
    }

    /// `true` iff the filtered cursor is an `INT32` measure-exponent literal
    /// ([`token_is_int32_exponent`]) that is *aligned* with the raw cursor —
    /// no LexFilter-swallowed `)` (`m^()` / `m^(1/)`) sits between, which would
    /// let the filtered cursor skip the closer and grab an outer literal.
    ///
    /// The kind check is on the *filtered* token, not the raw one, because a
    /// `sign_fold`-merged negative exponent (`m^ -2`, `m^(-1)`) is a single
    /// `Int("-2")` filtered token whereas the raw stream still has the separate
    /// `Op("-")` + digits — so requiring the *raw* cursor to be an int would
    /// wrongly reject the folded form. The swallowed-`)` guard is therefore a
    /// span-alignment test ([`Self::filtered_peek_aligned_with_raw`]): the
    /// folded literal's raw `-` shares the filtered token's start (aligned),
    /// while a swallowed `)` starts strictly before it (not aligned).
    fn at_int32_exponent(&self) -> bool {
        self.peek_is_int32_exponent() && self.filtered_peek_aligned_with_raw()
    }

    /// `true` iff no LexFilter-swallowed real token (a `)`) sits between the
    /// raw cursor and the filtered `peek()` — i.e. the next significant raw
    /// token does not start *before* the filtered token. A `sign_fold`-merged
    /// literal stays aligned (its raw sign shares the filtered start); a
    /// swallowed `)` does not (it precedes the next filtered token).
    fn filtered_peek_aligned_with_raw(&self) -> bool {
        let Some((_, fspan)) = self.peek() else {
            return false;
        };
        self.next_non_trivia_raw_at_pos_with_span()
            .is_some_and(|(_, rspan)| rspan.start >= fspan.start)
    }

    /// The `INT32 [/ INT32]` body shared by the signed and unsigned
    /// `rationalConstant` alternatives. Decides
    /// [`RATIONAL_CONST_RATIONAL`](SyntaxKind::RATIONAL_CONST_RATIONAL) vs
    /// [`RATIONAL_CONST_INTEGER`](SyntaxKind::RATIONAL_CONST_INTEGER) by a
    /// one-token lookahead for the `/` divisor.
    ///
    /// The `/` lookahead is gated on the **raw** stream, not the filtered one.
    /// LexFilter swallows the `)` that closes this `( … )` exponent (and any
    /// enclosing `(e : T)`), so a filtered-only lookahead would skip past those
    /// closers and mistake an *outer* `/` — `(x : m^(1)) / y`,
    /// `float<m^(1)/s>` — for a divisor, draining the `)` as `ERROR` and
    /// consuming tokens outside the exponent. The raw stream still carries the
    /// `)`, so requiring the next non-trivia raw token after the numerator to
    /// be `/` admits only a divisor *inside* the parens. The same raw gate on
    /// the denominator stops a `/` glued to a swallowed `)` (`m^(1/)…`) from
    /// crossing the closer.
    fn parse_rational_const_body(&mut self) {
        let cp = self.builder.checkpoint();
        // Numerator: raw-gated so an empty `m^()` (swallowed `)` at the cursor)
        // doesn't let the filtered peek cross the closer and grab an outer int.
        if !self.at_int32_exponent() {
            self.push_rational_const_error();
            return;
        }
        let num_end = self
            .peek()
            .map(|(_, span)| span.end)
            .unwrap_or(self.source.len());
        let is_rational = matches!(
            self.next_non_trivia_raw_after(num_end),
            Some(Token::Op("/"))
        );
        if is_rational {
            self.builder.start_node_at(
                cp,
                FSharpLang::kind_to_raw(SyntaxKind::RATIONAL_CONST_RATIONAL),
            );
            self.bump_rational_int(); // numerator
            self.bump_into(SyntaxKind::SLASH_TOK);
            // Denominator: raw-gate so a swallowed `)` immediately after the
            // `/` is never crossed (mirrors the numerator gate above).
            if self.at_int32_exponent() {
                // FCS reports a parse error (`parsIllegalDenominatorForMeasure
                // Exponent`) for a zero denominator but still builds the node;
                // mirror that — record the error, then bump so the shape and
                // the lossless text match.
                if let Some((Ok(FilteredToken::Raw(t)), span)) = self.peek().cloned()
                    && int32_exponent_is_zero(&t)
                {
                    self.errors.push(ParseError {
                        message: "denominator must not be 0 in unit-of-measure exponent"
                            .to_string(),
                        span,
                    });
                }
                self.bump_rational_int(); // denominator
            } else {
                self.push_rational_const_error();
            }
        } else {
            self.builder.start_node_at(
                cp,
                FSharpLang::kind_to_raw(SyntaxKind::RATIONAL_CONST_INTEGER),
            );
            self.bump_rational_int();
        }
        self.builder.finish_node();
    }

    /// `true` iff the filtered cursor is on an integer literal FCS admits as a
    /// unit-of-measure exponent — i.e. one that classifies to
    /// [`SyntaxKind::INT32_LIT`]: a decimal / hex / oct / bin literal, or a
    /// lowercase-`l`-suffixed Int32. A `1L` (Int64) or `1uy` (byte) suffix is
    /// a *different* terminal that FCS rejects in this position, so it is
    /// excluded here (the dispatch then records a recoverable error).
    fn peek_is_int32_exponent(&self) -> bool {
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(t)), _)) if token_is_int32_exponent(t)
        )
    }

    /// Bump the current measure-exponent integer literal as an
    /// [`SyntaxKind::INT32_LIT`], validating the value range first (FCS reports
    /// `lexOutsideThirtyTwoBitSigned` for out-of-range exponents but still
    /// builds the node, so the error is non-fatal). Decimal goes through
    /// [`validate_decimal_int`] and hex/oct/bin through [`validate_xint_int32`]
    /// — the same paths `parse_const_payload` uses; an `l`-suffixed Int32 was
    /// already range-checked by the [`Parser::peek_is_int32_exponent`] gate.
    /// The text may carry a [`super::sign_fold`]-merged `-`.
    fn bump_rational_int(&mut self) {
        if let Some((Ok(FilteredToken::Raw(t)), span)) = self.peek().cloned() {
            let range_err = match &t {
                Token::Int(text) => match validate_decimal_int(text) {
                    Ok(()) => None,
                    Err(DecimalIntError::Malformed) => {
                        Some(format!("malformed integer literal {text:?}"))
                    }
                    Err(DecimalIntError::OutOfRangeInt32) => Some(format!(
                        "integer literal {text:?} outside 32-bit signed range"
                    )),
                },
                Token::XInt(text) => validate_xint_int32(text)
                    .err()
                    .map(|()| format!("integer literal {text:?} outside 32-bit signed range")),
                _ => None,
            };
            if let Some(message) = range_err {
                self.errors.push(ParseError { message, span });
            }
        }
        self.bump_into(SyntaxKind::INT32_LIT);
    }

    /// Recovery for a malformed measure exponent (a non-`INT32` where the
    /// `rationalConstant` grammar requires one). Records a `ParseError` at the
    /// cursor; the partial `RATIONAL_CONST_*` node still closes so the
    /// lossless-text invariant holds.
    fn push_rational_const_error(&mut self) {
        let span = self
            .peek()
            .map(|(_, span)| span.clone())
            .unwrap_or_else(|| self.source.len()..self.source.len());
        self.errors.push(ParseError {
            message: "expected integer in unit-of-measure exponent".to_string(),
            span,
        });
    }

    /// Type variable — FCS's `SynType.Var(SynTypar, range)`, from the
    /// `typar` rule in `pars.fsy:6760-6768`. Shape:
    /// `VAR_TYPE > [sigil_kind, IDENT_TOK]` where `sigil_kind` is
    /// [`SyntaxKind::QUOTE_TOK`] for `'a` (`TyparStaticReq.None`) or
    /// [`SyntaxKind::HAT_TOK`] for `^T` (`TyparStaticReq.HeadType`). The
    /// caller has already verified the sigil is at `peek()`.
    pub(super) fn parse_var_type(&mut self, sigil_kind: SyntaxKind) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::VAR_TYPE));
        self.bump_into(sigil_kind);
        // Gate the ident lookahead on the next non-trivia *raw* token,
        // not just the filtered peek: a LexFilter-swallowed `)` between
        // the sigil and the next filtered token (e.g. `(x : ') y`) sits
        // before the filtered cursor's `y`, and consuming that outer
        // identifier as the typar name would drag the real `)` in as
        // `ERROR` and corrupt the surrounding parse. Mirrors the
        // raw-stream boundary checks in `parse_type` /
        // `parse_atomic_type`'s LPAREN arm.
        if self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Ident(_) | Token::QuotedIdent(_)))
        {
            self.bump_into(SyntaxKind::IDENT_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, span)| span.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected identifier after type-variable sigil".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    fn parse_anon_type(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ANON_TYPE));
        self.bump_into(SyntaxKind::UNDERSCORE_TOK);
        self.builder.finish_node();
    }

    /// `true` iff the next filtered token can start an expression.
    /// Currently: literal constants, bool keywords, identifiers (plain or
    /// backticked), unit literal `()`, and parenthesised expressions
    /// `( e )`. The `LParen` case needs lookahead into the *raw* stream
    /// (not the filtered stream) because the lexfilter swallows `RParen`
    /// to mirror FCS's outer-wrapper expansion to
    /// `RPAREN_*_COMING_SOON`/`RPAREN_IS_HERE` — we look at the first
    /// non-trivia raw after the LParen: `RParen` means unit;
    /// any token that itself starts an expression (including a nested
    /// `LParen`) means a paren-expression; anything else is rejected so
    /// `parse_paren_expr` doesn't get handed input the recursive
    /// `parse_expr` can't dispatch on.
    /// `true` if the cursor is at a comma that continues the *current*
    /// tuple — i.e. there is no LexFilter-swallowed `)` ahead of the
    /// comma in the raw stream.
    ///
    /// `RParen` is removed from the filtered stream by LexFilter, so a
    /// comma sitting after a closing paren can still appear as the next
    /// filtered token (peek-equivalence on the filtered side). When
    /// `parse_expr` is invoked recursively from `parse_paren_expr`, that
    /// outer comma belongs to the *enclosing* tuple, not this one;
    /// committing to a tuple here would consume the `)` as ERROR and
    /// then fail to find the close. Gate on the raw stream: only enter
    /// the tuple branch if the next non-trivia raw token *is* the
    /// comma.
    pub(super) fn at_tuple_continuation(&self) -> bool {
        let Some((Ok(FilteredToken::Raw(Token::Comma)), _)) = self.peek() else {
            return false;
        };
        // First non-trivia raw at-or-after `raw_pos`. A swallowed `)`
        // (or any other non-Comma raw) here means the comma is for a
        // surrounding construct, not us.
        for (res, _) in self.raw_tokens.iter().skip(self.raw_pos) {
            match res {
                Ok(t) if raw_is_trivia(t) => continue,
                Ok(TriviaToken::Lexed(Token::Comma)) => return true,
                _ => return false,
            }
        }
        false
    }

    /// `true` iff the raw cursor is positioned at a token that starts a
    /// type — FCS's `atomTypeOrAnonRecdType` (`pars.fsy:6520`), i.e.
    /// `atomType | anonRecdType`. The anon-recd branch admits both `{|`
    /// (bare) and `struct {|` (struct variant, `pars.fsy:2510-2522`);
    /// the latter requires a two-token lookahead because bare `struct`
    /// is also a legitimate token for unrelated constructs (e.g.
    /// `struct (int * int)`, which lives at a different grammar layer).
    /// Used as the recovery-gate predicate at the three sites that
    /// would otherwise accept a bare `struct` head and dispatch into
    /// [`Parser::parse_app_type`], which would then panic inside
    /// [`Parser::parse_atomic_type`]'s unreachable arm because neither
    /// `parse_anon_recd_type` (no `{|`) nor `parse_atomic_type` (no
    /// recognised starter) can absorb a bare `struct`.
    pub(super) fn peek_starts_type_or_anon_recd(&self) -> bool {
        // Reject when the filtered cursor sits on *any* virtual. Raw-stream
        // lookahead would happily skip the virtual and find a real type
        // starter past it, but `parse_atomic_type` keys off the filtered
        // `peek()` and would hit its `unreachable!` arm if we let the dispatch
        // through — and **no** virtual is ever a type starter (type starts are
        // always real tokens: ident, `(`, `_`, `'`/`^`, `#`, `{|`, `struct`).
        // The hazard shows up with several distinct virtuals depending on the
        // enclosing layout:
        //   * `Virtual::BlockSep` at the anon-recd "missing field type" surface
        //     `{| F :\n   G : string |}` (LexFilter sits one between F's colon
        //     and G), or an offside list-element `[ :?\n   int ]`;
        //   * `Virtual::End` when an offside line closes the enclosing context,
        //     e.g. `match x with :?\n  int -> 1` (the clause-list `End` lands
        //     between `:?` and the raw `int`).
        // We want `parse_type` to report a recoverable "expected type" error in
        // every such case, matching FCS, rather than panic.
        if matches!(self.peek(), Some((Ok(FilteredToken::Virtual(_)), _))) {
            return false;
        }
        // `atomTypeOrAnonRecdType = atomType | anonRecdType`. The `atomType`
        // half (incl. the phase-10.9 sign-folded `StaticConstant`) lives in the
        // shared [`Parser::peek_starts_atomic_type`]; this adds the anon-record
        // half (`{|` / `struct {|`).
        if self.peek_starts_atomic_type() {
            return true;
        }
        let Some((tok, span)) = self.next_non_trivia_raw_at_pos_with_span() else {
            return false;
        };
        if raw_starts_anon_recd_type(tok) {
            return true;
        }
        if matches!(tok, Token::Struct) {
            // `struct {|` — the anon-record struct variant. (The `struct (`
            // struct-tuple form is admitted above via `peek_starts_atomic_type`,
            // as it is an `atomType`; a bare `struct` before anything else is not
            // a type start.)
            return matches!(
                self.next_non_trivia_raw_after(span.end),
                Some(Token::LBraceBar)
            );
        }
        false
    }

    /// `true` if the cursor opens an FCS `atomType` (`pars.fsy:6534-6589`):
    /// ident / `(` / `_` / `'`/`^` / `#` / a static-constant literal-or-`null`-
    /// or-`const` head ([`raw_starts_atomic_type`]), **plus** a sign-folded
    /// signed literal (`-1`/`+1` → `StaticConstant`, phase 10.9). Raw-aligned and
    /// virtual-rejecting, so it never crosses a LexFilter-swallowed `)` nor lets
    /// `parse_atomic_type` reach its `unreachable!` arm.
    ///
    /// The fold needs a `&self` view because `raw_starts_atomic_type` only sees
    /// the *raw* token (`Op("-")` pre-fold) while the *filtered* cursor carries
    /// the merged `INT32_LIT`; we require the raw cursor be that `Op("-")` /
    /// `Op("+")` so a swallowed `)` (`(x : ) -1`) is not crossed. Shared by the
    /// `#T` hash-constraint recursion (so `#-1` → `HashConstraint(StaticConstant
    /// -1)`) and the `atomTypeOrAnonRecdType` gate above; the atomType
    /// production is strictly narrower than the latter (no anon record), which
    /// is why the `#` recursion uses *this* and not `peek_starts_type_or_anon_recd`
    /// (FCS rejects `#{| … |}`).
    pub(super) fn peek_starts_atomic_type(&self) -> bool {
        if matches!(self.peek(), Some((Ok(FilteredToken::Virtual(_)), _))) {
            return false;
        }
        // `struct (` opens a struct-tuple type, an `atomType` (its two-token
        // lookahead can't be expressed in the single-token `raw_starts_atomic_type`
        // below). The anon-record `struct {|` is *not* an `atomType`, so it is
        // deliberately not admitted here — `#{| … |}` / `#struct {| … |}` stay
        // rejected, matching FCS.
        if self.peek_starts_struct_tuple_type() {
            return true;
        }
        if let Some((Ok(FilteredToken::Raw(t)), _)) = self.peek()
            && token_is_folded_signed_literal(t)
            && self
                .next_non_trivia_raw_at_pos()
                .is_some_and(|r| matches!(r, Token::Op("-" | "+")))
        {
            return true;
        }
        self.next_non_trivia_raw_at_pos()
            .is_some_and(raw_starts_atomic_type)
    }

    /// `true` if the cursor opens a *leading*-`/` measure-division tuple
    /// (`float</s>`, `(x : /s)`) — FCS's `INFIX_STAR_DIV_MOD_OP
    /// tupleOrQuotTypeElements` (`pars.fsy:6262`, phase 10.9), a `typ` with a
    /// leading `SynTupleTypeSegment.Slash` and no head type. Raw-aligned: the
    /// raw cursor must be `Op("/")` (guarding a LexFilter-swallowed `)`) and the
    /// filtered cursor the same `/`.
    ///
    /// This is a `typ`-*level* start (consumed by [`Parser::parse_tuple_type`]),
    /// **not** an `atomType` start — so it is deliberately kept out of
    /// [`Parser::peek_starts_type_or_anon_recd`], whose post-separator caller in
    /// `parse_tuple_type` recurses into the atomic-level
    /// `parse_app_type_can_be_nullable` and would panic on a bare `/`.
    pub(super) fn peek_leading_slash(&self) -> bool {
        self.next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Op("/")))
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Op("/"))), _))
            )
    }

    /// `true` if a full `typ` starts at the cursor — an `atomType` start (incl.
    /// a sign-folded literal, [`Parser::peek_starts_type_or_anon_recd`]) **or** a
    /// leading `/` ([`Parser::peek_leading_slash`]). Use at the callers that gate
    /// then invoke [`Parser::parse_type`] (the typed-paren entry and the
    /// paren-type inner). The atomic-level callers (postfix factor, union-case
    /// field, `:?` IsInst, `open type`) keep `peek_starts_type_or_anon_recd`,
    /// which rejects a leading `/`.
    pub(super) fn peek_starts_type(&self) -> bool {
        self.peek_starts_type_or_anon_recd() || self.peek_leading_slash()
    }

    /// `true` if [`Parser::parse_atomic_expr`] can consume the cursor as the
    /// `const` operand of a `StaticConstantExpr` (FCS's `CONST atomicExpr`,
    /// `pars.fsy:6583`) without hitting its `parse_const_payload` /
    /// LParen-dispatch `unreachable!` arms, raw-aligned against a swallowed `)`.
    ///
    /// The set is exactly `parse_atomic_expr`'s safe input: a const-payload
    /// literal / ident / quote / brace / interp / prefix-op
    /// ([`raw_starts_atomic_expr`]) or a sign-folded literal
    /// ([`token_is_folded_signed_literal`]); and for a `(` head, the dispatch
    /// peers past it — the inside must be `)` (unit) or a minus-expr starter
    /// (paren expr), else `parse_atomic_expr` would `unreachable!` (e.g.
    /// `const (>`). The non-`(` arm also requires the raw cursor be a matching
    /// atomic start (or the `-`/`+` of a fold), so `(x : const) y`'s swallowed
    /// `)` is not crossed.
    pub(super) fn peek_starts_const_arg_expr(&self) -> bool {
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::LParen)), lparen_span)) => {
                let after = lparen_span.end;
                // Raw-align first: a LexFilter-swallowed `)` can sit between
                // `const` and this filtered `(` (e.g. `(x : const) (1)` — the
                // raw cursor is the outer-paren `)`, the filtered cursor the
                // inner `(`). Require the raw cursor to be the `(` itself so the
                // missing-operand error fires at the in-paren boundary instead
                // of draining the `)` as ERROR and stealing `(1)`. Then peer
                // past the `(` exactly as `parse_atomic_expr`'s LParen arm does.
                matches!(self.next_non_trivia_raw_at_pos(), Some(Token::LParen))
                    && self
                        .next_non_trivia_raw_after(after)
                        .is_some_and(|t| matches!(t, Token::RParen) || raw_starts_minus_expr(t))
            }
            Some((Ok(FilteredToken::Raw(t)), _))
                if raw_starts_atomic_expr(t) || token_is_folded_signed_literal(t) =>
            {
                self.next_non_trivia_raw_at_pos()
                    .is_some_and(|r| raw_starts_atomic_expr(r) || matches!(r, Token::Op("-" | "+")))
            }
            _ => false,
        }
    }
}
