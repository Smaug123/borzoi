//! Atomic expression productions: the atomic dispatch, constant literals
//! (`parse_const_payload`), identifiers, parenthesised expressions, quotes,
//! `yield`/`return`, `do!`, and brace expressions (record / computation).

use super::decls_type::TypeAltOperand;
use super::*;

/// Which `braceExprBody` form a non-`new`-headed `{ … }` is, decided by
/// lookahead in [`Parser::classify_brace_body`]. A `new`-headed brace (an object
/// expression `{ new T with member … }` or a computation expression wrapping a
/// construction `{ new T(args) }`) is diverted earlier by
/// [`Parser::parse_brace_expr`] to [`Parser::parse_obj_or_computation_brace`], so
/// it never reaches this classifier.
#[derive(Clone, Copy)]
enum BraceBody {
    /// `{ F = e; … }` (field list) or `{ src with F = e; … }` (copy-update)
    /// whose source is a *bare longident* — the lookahead-decidable record forms.
    Record { copy_update: bool },
    /// An ident-headed brace whose head longident continues as an `appExpr`
    /// (application args / postfix): `{ f x … }`, `{ Foo.Bar () … }`. FCS's
    /// `recdExprCore: appExpr WITH …` admits *any* `appExpr` copy source, but the
    /// appExpr's `)`/`}` closers are LexFilter-swallowed, so a bare lookahead
    /// cannot tell whether a trailing `with` follows. Resolved by
    /// [`Parser::parse_app_head_brace`], which parses the leading `appExpr` for
    /// real and then picks copy-update record (a `with` follows) versus
    /// computation expression — mirroring the `new`-headed
    /// [`Parser::parse_obj_or_computation_brace`] checkpoint discipline.
    AppExprHead,
    /// A computation expression `{ e }` / `seq { … }` (the catch-all).
    Computation,
}

impl<'src> Parser<'src> {
    /// Parse one atomic expression — the level below tuple-formation —
    /// followed by any postfix tail ([`Self::parse_postfix_tail`]) — FCS's
    /// left-recursive `atomicExpr`: the high-precedence (adjacent) application
    /// `f(x)`, member access `expr.Member` (`DotGet`), and dotted index
    /// `expr.[index]` (`DotIndexedGet`), interleaved left-associatively. The
    /// head dispatch lives in [`Self::parse_atomic_expr_head`].
    pub(super) fn parse_atomic_expr(&mut self) {
        // Depth-guarded: with `parse_minus_expr`, one of the two universal
        // expression chokepoints. Every nesting level of the main cycle passes
        // through here (paren / brace / CE / app-argument atoms), and the
        // atomic-level prefix `!`/`~` chains (`! ! … x`) re-enter here per
        // operator — a path that bypasses `parse_minus_expr`. Guarding the body
        // bounds them all; the recursion re-enters this public wrapper.
        self.with_depth(Self::parse_atomic_expr_inner);
    }

    fn parse_atomic_expr_inner(&mut self) {
        // Checkpoint the head so the postfix tail can splice it under an
        // `APP_EXPR` / `DOT_GET_EXPR` / `DOT_INDEXED_GET_EXPR` wrapper. With no
        // postfix following, the checkpoint goes unused and the green tree is
        // byte-identical to the bare head.
        let cp = self.builder.checkpoint();
        // The head's first-token start byte — the object's start for a trailing
        // `.( :: ).<int>` cons-field qualification, whose library-only diagnostic
        // FCS anchors on the *whole* `obj.( :: ).<int>` expression.
        let head_start = self.peek().map(|(_, s)| s.start);
        // A bare operator-value head (`(+)`, `(*)`) folds a trailing
        // `.member`/`.(op)` qualification into its *own* long-ident here — FCS's
        // `mkSynDot` appends to the `SynExpr.LongIdent` an `opName` produces, so
        // `(+).Bar` is the single `LongIdent(["+"; "Bar"])`, exactly like the
        // ident-rooted `a.b.c` path. This must happen in the full-expression
        // context, **not** in the head-only [`Self::parse_atomic_expr_head`]
        // that [`Self::parse_postfix_tail`] reuses for high-precedence paren-app
        // arguments: there the `.Bar` of `f(+).Bar` must bind to the whole
        // application (`DotGet(App(f, (+)), [Bar])`), so the `(+)` argument stays
        // head-only (`fold = false`).
        if self.at_paren_op_value(self.pos) {
            self.parse_paren_op_value_expr(true);
        } else if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LParenStarRParen)), _))
        ) {
            self.parse_star_op_value_expr(true);
        } else if self.at_active_pat_name() {
            // A bare active-pattern-name value `(|Foo|_|)` — FCS's `identExpr:
            // opName`, the same single-segment `SynExpr.LongIdent` an
            // operator-value produces (`fold = true` so `(|Foo|_|).Bar` folds
            // its trailing `.member` onto the name, as `(+).Bar` does).
            self.parse_active_pat_name_expr(true);
        } else {
            self.parse_atomic_expr_head();
        }
        self.parse_postfix_tail(cp, head_start);
    }

    /// The atomic-expression head dispatch, before the postfix dot/index
    /// tail. Dispatches on the leading filtered token's kind: const literals
    /// open `CONST_EXPR`; `Ident`/`QuotedIdent` open `IDENT_EXPR` (FCS's
    /// optimised representation for a single-segment `SynLongIdent`,
    /// `SyntaxTree.fsi:805`); `null` opens `NULL_EXPR` (FCS's `SynExpr.Null`,
    /// a distinct atom — *not* a `SynConst`). `LParen` routes to either the
    /// unit-literal arm of `parse_const_expr` (empty parens with at most trivia
    /// inside) or `parse_paren_expr` (parens wrapping an inner expression).
    ///
    /// Exposed for the high-precedence paren-app argument in
    /// [`Self::parse_postfix_tail`], which parses the `(x)` of `f(x)` *without*
    /// its own postfix tail: a trailing `(y)` / `.Bar` chains onto the whole
    /// application via that loop, not onto the argument (`f(x).Bar` =
    /// `DotGet(App(f, (x)), …)`).
    pub(super) fn parse_atomic_expr_head(&mut self) {
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Raw(Token::Op(text))), _)) if is_prefix_op_text(text) => {
                self.parse_prefix_op_app();
            }
            Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), _)) => {
                self.parse_ident_expr();
            }
            // `base.Member` — base-class member access (FCS's `BASE DOT
            // atomicExprQualification`). The `base` keyword is its own token, but
            // FCS treats it as `Ident("base")` heading a long-ident path; a `.`
            // qualification is mandatory (a bare `base` is a parse error).
            Some((Ok(FilteredToken::Raw(Token::Base)), _)) => {
                self.parse_base_expr();
            }
            // `global` / `global.Path` — the global-namespace root (FCS's
            // `GLOBAL DOT …`). The `global` keyword is its own token, but FCS
            // treats it as an identifier heading a long-ident path; unlike
            // `base`, a `.` qualification is *optional* (a bare `global` is
            // valid).
            Some((Ok(FilteredToken::Raw(Token::Global)), _)) => {
                self.parse_global_expr();
            }
            Some((Ok(FilteredToken::Raw(Token::InterpString(_))), _)) => {
                self.parse_interp_string_expr();
            }
            Some((Ok(FilteredToken::Raw(Token::LQuote | Token::LQuoteRaw)), _)) => {
                self.parse_quote_expr();
            }
            // `begin … end` — the verbose-syntax parenthesis (`beginEndExpr`,
            // `pars.fsy:5419`). `begin e end` → `SynExpr.Paren e`; the empty
            // `begin end` → `SynConst.Unit`.
            Some((Ok(FilteredToken::Raw(Token::Begin)), _)) => {
                self.parse_begin_end_expr();
            }
            Some((Ok(FilteredToken::Raw(Token::LBrace)), _)) => {
                self.parse_brace_expr();
            }
            Some((Ok(FilteredToken::Raw(Token::LBraceBar)), _)) => {
                self.parse_anon_recd_expr(false);
            }
            Some((Ok(FilteredToken::Raw(Token::LBrack | Token::LBrackBar)), _)) => {
                self.parse_array_or_list_expr();
            }
            Some((Ok(FilteredToken::Raw(Token::Struct)), struct_span)) => {
                // `struct (…)` → struct tuple; `struct {| … |}` → struct
                // anon-record. A `struct` followed by neither is a clean error.
                match self.filtered_tokens.get(self.pos + 1) {
                    Some((Ok(FilteredToken::Raw(Token::LParen)), _)) => {
                        self.parse_struct_tuple_expr();
                    }
                    Some((Ok(FilteredToken::Raw(Token::LBraceBar)), _)) => {
                        self.parse_anon_recd_expr(true);
                    }
                    _ => {
                        self.errors.push(ParseError {
                            message: "expected `(` or `{|` after `struct` in an expression"
                                .to_string(),
                            span: struct_span,
                        });
                        // Consume the `struct` so the round-trip stays lossless
                        // and the caller's loop cannot spin on it.
                        self.bump_into(SyntaxKind::STRUCT_TOK);
                    }
                }
            }
            Some((Ok(FilteredToken::Raw(Token::Null)), _)) => {
                self.parse_null_expr();
            }
            // `'T` — the F# 7 typar expression (FCS's `QUOTE ident` →
            // `SynExpr.Typar`). The `Char` regex has already claimed a real char
            // literal (`'a'`), so a `Quote` at the atom head is the sigil of a
            // typar-expr; `parse_typar_expr` consumes `'` + the name (or reports
            // a clean error for a bare `'`). A trailing `.Member` / `(args)` is
            // left for the postfix tail.
            Some((Ok(FilteredToken::Raw(Token::Quote)), _)) => {
                self.parse_typar_expr();
            }
            // `_.member` — the accessor-function shorthand (FCS's
            // `UNDERSCORE DOT atomicExpr`, `AccessorFunctionShorthand`). Only a
            // `_` *followed by a `.`* opens a dot-lambda; a bare `_` in
            // expression position is an error (FCS's `UNDERSCORE recover`
            // `FromParseError(Ident "_")`, which we don't model), so it falls
            // through to the const-expr error arm below. Gated on the two-token
            // shape via [`Self::at_dot_lambda`] (the per-token classifiers can't
            // do that lookahead, so `Underscore` stays out of
            // `raw_starts_atomic_expr`).
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _))
                if self.at_dot_lambda(self.pos) =>
            {
                self.parse_dot_lambda_expr();
            }
            // Glued `(*)` — the multiply operator-value (a single lexer token,
            // distinct from the spaced `( * )` wildcard). FCS's `op_Multiply`
            // (`pars.fsy:6806`). Head-only (`fold = false`): this dispatch is
            // reused for HPA arguments (`f(*)`), where a trailing `.member`
            // binds to the whole application, not the argument. Bare-position
            // folding happens in [`Self::parse_atomic_expr`].
            Some((Ok(FilteredToken::Raw(Token::LParenStarRParen)), _)) => {
                self.parse_star_op_value_expr(false);
            }
            // A general parenthesised operator-value `( op )` (FCS's
            // `identExpr: opName`). Checked before the unit / paren-expr
            // dispatch so `(-)`, `(..)`, … reinterpret as the operator-value
            // (their operators are otherwise prefix / range starters that would
            // route to `parse_paren_expr`). The immediate-`)` requirement in
            // [`Self::at_paren_op_value`] keeps `(- x)` / `(..3)` as paren
            // expressions. Head-only (`fold = false`) — see the `(*)` arm.
            Some((Ok(FilteredToken::Raw(Token::LParen)), _))
                if self.at_paren_op_value(self.pos) =>
            {
                self.parse_paren_op_value_expr(false);
            }
            // An active-pattern-name value `(|Foo|_|)` — FCS's `identExpr:
            // opName`, like the operator-value arm above. Detected by a bare `|`
            // right after `(` ([`Self::at_active_pat_name`], disjoint from the
            // operator-value names, whose `|` glues into `Op` / `BarBar`).
            // Checked before the unit / paren-body arm so `( |…` reads as the
            // name, not a parenthesised expression. Head-only (`fold = false`) —
            // see the operator-value arm: a trailing `.member` on an HPA
            // argument binds to the whole application, not the name.
            Some((Ok(FilteredToken::Raw(Token::LParen)), _)) if self.at_active_pat_name() => {
                self.parse_active_pat_name_expr(false);
            }
            Some((Ok(FilteredToken::Raw(Token::LParen)), lparen_span)) => {
                // Lexfilter swallows `RParen` (see `peek_is_expr_start`),
                // so unit-vs-paren-expr dispatch peers past it in the raw
                // stream: `RParen` means unit, a minus-expr starter means
                // a paren-wrapped expression. `peek_is_expr_start` has
                // already filtered out everything else, so the wildcard
                // is unreachable.
                match self.next_non_trivia_raw_after(lparen_span.end) {
                    // `(#` opens FSharp.Core's inline-IL expression
                    // `(# "instr" … #)` (FCS's `inlineAssemblyExpr`, a
                    // `parenExprBody`). Checked before the unit / paren-body
                    // arms because a `#` after `(` is unambiguously inline IL,
                    // not an ordinary parenthesised expression body.
                    // `parse_inline_il_expr` still emits the `PAREN_EXPR`
                    // wrapper (FCS's `Paren(LibraryOnlyILAssembly)`).
                    // `raw_after_lparen_starts_expr` admits `Hash` too, so the
                    // gating lookaheads agree this `(` starts an atom.
                    Some(Token::Hash) => self.parse_inline_il_expr(),
                    Some(Token::RParen) => self.parse_const_expr(),
                    // Everything else `peek_is_expr_start` admitted — a
                    // paren-wrapped expression body (incl. a block `let`/`use`,
                    // `(let a = 1 in a)`). The `RParen` (unit) case is caught
                    // above, so the shared `(`-after predicate's `RParen` arm is
                    // unreachable here; using it keeps this dispatch in lockstep
                    // with the lookaheads that admitted the `(`.
                    Some(t) if raw_after_lparen_starts_expr(t) => self.parse_paren_expr(),
                    other => unreachable!(
                        "parse_atomic_expr LParen dispatch: peek_is_expr_start should have filtered {other:?}",
                    ),
                }
            }
            // `?ident` — the caller-side optional named argument (FCS's
            // `QMARK nameop` → `SynExpr.LongIdent(isOptional = true, …)`). A
            // *prefix* `?` at the atom head (no preceding expr) is unambiguously
            // this form; the *postfix* dynamic `a?b` is handled in the postfix
            // tail ([`Self::parse_dynamic_tail`]) after an atom, never reaching
            // this head dispatch. Gated on a following nameop so a bare `?` falls
            // through to the const-expr error arm.
            Some((Ok(FilteredToken::Raw(Token::QMark)), _)) if self.qmark_opens_optional_arg() => {
                self.parse_optional_named_arg_expr();
            }
            _ => self.parse_const_expr(),
        }
    }

    /// `true` when the cursor's `?` opens an optional-named-argument expression
    /// (`?ident`). Convenience for [`Self::qmark_opens_optional_arg_at`] at the
    /// cursor.
    pub(super) fn qmark_opens_optional_arg(&self) -> bool {
        self.qmark_opens_optional_arg_at(self.pos)
    }

    /// `true` when the filtered token at `idx` is a `?` opening an
    /// optional-named-argument expression (`?ident`) — FCS's `QMARK nameop`
    /// (`pars.fsy:5280`). The identifier must immediately follow the `?` in
    /// **both** streams, ruling out the two dual-stream hazards:
    ///
    /// * **filtered** — an intervening layout virtual (`?⏎opt` across an offside
    ///   break) sits in the filtered stream but not the raw; requiring a filtered
    ///   ident stops the parse bumping that virtual as the name.
    /// * **raw** — a LexFilter-swallowed closer (`(1 + ?) opt`, where the `)`
    ///   between `?` and `opt` is gone from the filtered stream) sits in the raw
    ///   stream; requiring a raw ident stops the parse draining that `)` and
    ///   stealing `opt` from the enclosing construct.
    ///
    /// `nameop` also admits an operator name, but only the identifier form occurs
    /// in real optional-arg code, so an operator after `?` stays a clean error (a
    /// follow-up). Mirrors [`Self::at_dot_lambda`]'s offset-based two-token shape.
    pub(super) fn qmark_opens_optional_arg_at(&self, idx: usize) -> bool {
        let Some((Ok(FilteredToken::Raw(Token::QMark)), qmark_span)) =
            self.filtered_tokens.get(idx)
        else {
            return false;
        };
        matches!(
            self.next_non_trivia_filtered_after_index(idx),
            Some(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_)))
        ) && matches!(
            self.next_non_trivia_raw_after(qmark_span.end),
            Some(Token::Ident(_) | Token::QuotedIdent(_))
        )
    }

    /// Parse a prefix `?ident` optional named argument → `LONG_IDENT_EXPR >
    /// [QMARK_TOK, LONG_IDENT > [IDENT_TOK]]`, mirroring FCS's
    /// `SynExpr.LongIdent(isOptional = true, SynLongIdent([ident]), …)`. The `?`
    /// rides as a marker token (the normaliser projects only the `LONG_IDENT`
    /// segments, eliding `isOptional` exactly as FCS's projection does). A
    /// trailing `.member` / application is left for the postfix tail. Caller has
    /// verified [`Self::qmark_opens_optional_arg`].
    fn parse_optional_named_arg_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_EXPR));
        self.bump_into(SyntaxKind::QMARK_TOK);
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        self.bump_into(SyntaxKind::IDENT_TOK);
        self.builder.finish_node(); // LONG_IDENT
        self.builder.finish_node(); // LONG_IDENT_EXPR
    }

    /// The whole-dimension wildcard `*` → [`SyntaxKind::INDEX_RANGE_EXPR`]`>
    /// [STAR_TOK]` (FCS's `SynExpr.IndexRange(None, None)`, phase 10.22a). A
    /// complete range with **no** `DOT_DOT_TOK` and no bound children, so the
    /// facade `IndexRangeExpr::lower()/upper()` both return `None` and it
    /// projects to `IndexRange { None, None }` — byte-matching FCS without a new
    /// node or normaliser arm. Emitted at the `minusExpr` level
    /// ([`Parser::parse_minus_expr`]), **not** the atom level: FCS's `STAR` is a
    /// `declExpr` leaf, not an `atomicExpr`, so it must not pick up an
    /// application argument or a postfix `.member`/`.[i]` tail (`* x` /
    /// `*.Length` are FCS errors). The caller has verified the cursor is the
    /// lone `Op("*")`.
    pub(super) fn parse_index_wildcard(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INDEX_RANGE_EXPR));
        self.bump_into(SyntaxKind::STAR_TOK);
        self.builder.finish_node();
    }

    /// `pars.fsy:5402 atomicExprAfterType: NULL { SynExpr.Null(lhs …) }` —
    /// the `null` literal expression. Emits `NULL_EXPR > [NULL_TOK]`.
    /// FCS keeps `null` distinct from `SynConst` (it is its own
    /// `atomicExpr` alternative, at `TRUE`/`FALSE` precedence), so this
    /// gets a dedicated node rather than riding on `CONST_EXPR`. The
    /// caller (`parse_atomic_expr`) has already verified the leading
    /// token is `Token::Null`.
    pub(super) fn parse_null_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::NULL_EXPR));
        self.bump_into(SyntaxKind::NULL_TOK);
        self.builder.finish_node();
    }

    /// `pars.fsy:5263 QUOTE ident` — the F# 7 typar expression `'T`
    /// (`SynExpr.Typar(SynTypar(id, TyparStaticReq.None, false), range)`), a
    /// type parameter used as an expression. Emits `TYPAR_EXPR > [QUOTE_TOK,
    /// IDENT_TOK]`. The caller ([`Self::parse_atomic_expr_head`]) has verified
    /// the leading token is `Token::Quote`; the postfix tail then chains a
    /// `.Member` (`DotGet`) and `(args)` (high-precedence app) onto it, so
    /// `'T.op_Addition(x, y)` becomes `App(DotGet(Typar 'T, [op_Addition]),
    /// Paren(Tuple[x; y]))`.
    ///
    /// The name is gated on the *filtered* cursor being an ident: FCS's `QUOTE
    /// ident` requires the two to be immediately adjacent (whitespace aside), so
    /// a LexFilter layout virtual landing between them — the offside break in
    /// `let f =⏎    '⏎    T` inserts a `Virtual::BlockSep` there — means FCS
    /// reports a parse error. Gating on the raw stream alone (which skips the
    /// virtual and finds the later `T`) would `bump_into` that virtual as a
    /// zero-width `IDENT_TOK` and silently accept, so the filtered peek must be
    /// the ident itself; a bare `'`, or a `'` split from its name by a virtual,
    /// records a clean recoverable error. Only the quote sigil reaches here — a
    /// `^`-sigil `^T.M` is FCS's `IndexFromEnd` (the `^` from-end index prefix),
    /// handled by the minus-level prefix path, not this atom.
    pub(super) fn parse_typar_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPAR_EXPR));
        self.bump_into(SyntaxKind::QUOTE_TOK);
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                _
            ))
        ) {
            self.bump_into(SyntaxKind::IDENT_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected identifier after type-variable sigil".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // TYPAR_EXPR
    }

    /// `true` if the filtered token at `idx` is `_` whose next significant
    /// *raw* token is a `.` — the head of the accessor-function shorthand
    /// `_.member` (FCS's `UNDERSCORE DOT atomicExpr`, `pars.fsy:5212`). The
    /// raw-stream probe (rather than the filtered stream) is load-bearing: it
    /// admits the spaced `_ .Foo` form FCS accepts (whitespace is trivia) while
    /// rejecting a `.` reached only by crossing a LexFilter-swallowed closer —
    /// in `(_).Foo` the `)` is gone from the *filtered* stream, so a filtered
    /// probe would see the outer `.` and wrongly drag `.Foo` into a dot-lambda;
    /// the raw stream still carries the `)`, so the next raw token after `_` is
    /// the closer (not `.`) and `_` stays a bare (error) atom inside the paren.
    /// The two-token shape is *required*: a bare `_` is not an expression, so
    /// the expression-start gates and the atomic-head dispatch all condition on
    /// this rather than treating `Underscore` as an unconditional atom starter.
    pub(super) fn at_dot_lambda(&self, idx: usize) -> bool {
        let Some((Ok(FilteredToken::Raw(Token::Underscore)), us_span)) =
            self.filtered_tokens.get(idx)
        else {
            return false;
        };
        matches!(
            self.next_non_trivia_raw_after(us_span.end),
            Some(Token::Dot),
        )
    }

    /// `pars.fsy:5212 atomicExpr: UNDERSCORE DOT atomicExpr %prec dot_lambda` —
    /// the accessor-function shorthand `_.member` (`SynExpr.DotLambda`,
    /// `LanguageFeature.AccessorFunctionShorthand`). `_.Foo` is sugar for
    /// `(fun x -> x.Foo)`; the synthesised parameter is introduced later (at
    /// type-check time), so the parse tree carries only the *body*. Emits
    /// `DOT_LAMBDA_EXPR > [UNDERSCORE_TOK, DOT_TOK, <body-expr>]`.
    ///
    /// The body is a full [`Self::parse_atomic_expr`] (FCS's `atomicExpr` after
    /// the `DOT`), so the member-chain folding, high-precedence application, and
    /// indexer tails fall out of the reused atom parse: `_.Foo.Bar` →
    /// `DotLambda(LongIdent ["Foo"; "Bar"])`, `_.Item(3)` →
    /// `DotLambda(App(Atomic, Ident "Item", Paren …))`. Because the body greedily
    /// consumes the whole postfix chain, the caller's
    /// [`Self::parse_postfix_tail`] finds no adjacent `.` to attach, so the
    /// chain binds *inside* the dot-lambda (not `DotGet(DotLambda …, …)`),
    /// matching the `%prec dot_lambda` (below `DOT`) precedence. The caller has
    /// verified [`Self::at_dot_lambda`].
    pub(super) fn parse_dot_lambda_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::DOT_LAMBDA_EXPR));
        self.bump_into(SyntaxKind::UNDERSCORE_TOK);
        // `bump_into` drains the `_`↔`.` trivia (the spaced `_ .Foo` form), and
        // a following `parse_atomic_expr` likewise absorbs the `.`↔member
        // trivia, so both whitespace variants attach inside the node.
        self.bump_into(SyntaxKind::DOT_TOK);
        // `at_dot_lambda` only guaranteed the `_.` head, not a body — `_.` at
        // EOF or before a delimiter (`let f = _.`, `List.map _.`) has none.
        // `parse_atomic_expr` assumes its caller verified an atomic starter
        // (it bottoms out in `parse_const_payload`'s `unreachable!`), so guard
        // it here. This is FCS's `UNDERSCORE DOT recover` arm (`pars.fsy:5221`):
        // a recovered `DotLambda` with a placeholder body and a parse error,
        // rather than a panic — keeping the round-trip lossless and the LSP
        // alive on a half-typed shorthand.
        //
        // The body must also be raw-adjacent: in `(_.) x` the `)` is swallowed
        // from the filtered stream, so a bare `peek_starts_atomic_expr` would
        // see the *outside* `x` and drag it in as the body. The raw-adjacency
        // check (the cursor's token is the next significant raw token, no
        // swallowed closer between the `.` and it) leaves `)` and `x` to the
        // enclosing paren and recovers at `_.` instead.
        if self.peek_starts_atomic_expr() && self.cursor_is_raw_adjacent() {
            self.parse_atomic_expr();
        } else {
            self.push_missing_operand_error_with("expected expression after `_.`");
        }
        self.builder.finish_node();
    }

    /// `pars.fsy:5433 quoteExpr: LQUOTE typedSequentialExpr RQUOTE` — a
    /// code quotation `<@ e @>` (typed) or `<@@ e @@>` (raw). Emits
    /// `QUOTE_EXPR > [LQUOTE_TOK, <inner-body>, RQUOTE_TOK]`. The inner is a
    /// `typedSequentialExpr` block ([`Self::parse_seq_block_body`]) — `;` /
    /// offside-separated statements plus an optional trailing `: T`, matching
    /// FCS's `quoteExpr: LQUOTE typedSequentialExpr RQUOTE` (`pars.fsy:5434`);
    /// the closer is a plain `RQuote`/`RQuoteRaw` (LexFilter has already split
    /// the compound `@>.` / `@>|}` forms into a bare closer plus `.` / `|}`).
    ///
    /// `isRaw` is taken from the *opener* and recovered later from the
    /// `LQUOTE_TOK` text. FCS's action (`pars.fsy:5436`) reports
    /// `parsMismatchedQuote` when `$1 <> $3` (opener and closer disagree
    /// on raw-ness) but still builds `SynExpr.Quote(_, snd $1, …)` with the
    /// opener's flag; we mirror both halves — push a parse error on the
    /// mismatch, keep the opener-derived `isRaw`.
    pub(super) fn parse_quote_expr(&mut self) {
        // Capture the opener's raw-ness and span before bumping.
        let (opener_is_raw, opener_span) = match self.peek() {
            Some((Ok(FilteredToken::Raw(t @ (Token::LQuote | Token::LQuoteRaw))), span)) => {
                (matches!(t, Token::LQuoteRaw), span.clone())
            }
            _ => unreachable!("parse_quote_expr entered without a quotation opener"),
        };
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::QUOTE_EXPR));
        self.bump_into(SyntaxKind::LQUOTE_TOK);
        // Drain trivia between the opener and the inner expression so it
        // attaches to `QUOTE_EXPR` rather than the inner expr's node —
        // symmetric to `parse_paren_expr`.
        if let Some((_, next_span)) = self.peek() {
            let start = next_span.start;
            self.drain_raw_up_to(start);
        }
        // FCS's `quoteExpr` body is a `typedSequentialExpr` (`pars.fsy:5434`), so
        // it is the full statement-sequence surface — `;` / offside-`OBLOCKSEP`
        // separated statements and an optional trailing `: T` — not a single
        // expression. The `@>` / `@@>` closer is neither an expr-start nor a
        // separator, so the gatherer stops at it cleanly. `parse_seq_block_body`
        // also emits the missing-first-statement error when the body is empty.
        self.parse_seq_block_body("expected expression inside quotation");
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Raw(tok @ (Token::RQuote | Token::RQuoteRaw))), span)) => {
                if matches!(tok, Token::RQuoteRaw) != opener_is_raw {
                    self.errors.push(ParseError {
                        message: "mismatched quotation delimiters".to_string(),
                        span,
                    });
                }
                self.bump_into(SyntaxKind::RQUOTE_TOK);
            }
            _ => {
                self.errors.push(ParseError {
                    message: "unmatched quotation; expected a closing `@>` / `@@>`".to_string(),
                    span: opener_span,
                });
            }
        }
        self.builder.finish_node();
    }

    /// `pars.fsy:4488 YIELD declExpr` (`yield`/`return`) and `pars.fsy:4510
    /// YIELD_BANG declExpr` (`yield!`/`return!`) — the computation-expression
    /// control-flow prefixes. Emits `YIELD_OR_RETURN[_FROM]_EXPR >
    /// [YIELD[_BANG]_TOK, <inner-expr>]`. The body is the full `declExpr`
    /// surface (`parse_expr`). `isYield` is recovered downstream from the
    /// keyword token text; the `(isYield, !isYield)` flag pair is
    /// reconstructed by the normaliser. `do!` is *not* handled here — it
    /// carries offside scaffolding and lands with the binder slice.
    pub(super) fn parse_yield_or_return(&mut self, is_from: bool) {
        let (node_kind, tok_kind) = if is_from {
            (
                SyntaxKind::YIELD_OR_RETURN_FROM_EXPR,
                SyntaxKind::YIELD_BANG_TOK,
            )
        } else {
            (SyntaxKind::YIELD_OR_RETURN_EXPR, SyntaxKind::YIELD_TOK)
        };
        self.builder.start_node(FSharpLang::kind_to_raw(node_kind));
        self.bump_into(tok_kind);
        if self.peek_is_expr_start() {
            let expr_cp = self.builder.checkpoint();
            self.parse_expr();
            // `YIELD declExpr COLON typ` (FCS `pars.fsy:4488`, and the `yield!`
            // variant): an optional `: T` binds the yielded `declExpr` *inside*
            // the yield — `yield e : T` is `YieldOrReturn(Typed(e, T))`, not
            // `Typed(YieldOrReturn(e), T)`. This is the only typed-annotation
            // surface in a list/array element (`[1 : int]` is rejected — bracket
            // elements are a bare `sequentialExpr`), and it also applies in a
            // computation-expression brace body. Uses the shared dual
            // filtered/raw gate so an offside outer annotation parked behind the
            // yield's close-virtuals is left for the enclosing body.
            if self.at_typed_annotation_colon() {
                self.builder
                    .start_node_at(expr_cp, FSharpLang::kind_to_raw(SyntaxKind::TYPED_EXPR));
                self.bump_into(SyntaxKind::COLON_TOK);
                self.parse_type();
                self.builder.finish_node();
            }
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected expression after `yield` / `return`".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// `pars.fsy:4613 ODO_BANG typedSequentialExprBlock` — a `do! e` in a
    /// computation expression. Emits `DO_BANG_EXPR > [DO_BANG_TOK,
    /// ERROR(BlockBegin), <body>, ERROR(BlockEnd), ERROR(DeclEnd)]`.
    ///
    /// LexFilter rewrites the raw `Token::DoBang` to `Virtual::DoBang`
    /// (raw still at `raw_pos`, same span) and wraps the body in a SeqBlock,
    /// so the mechanics mirror the `if`/`then` body: emit the keyword text
    /// from the virtual (the `THEN_TOK`-style direct path), consume the
    /// `Virtual::BlockBegin`/`BlockEnd` scaffolding via [`Self::parse_if_body`]
    /// as zero-width `ERROR`, then consume the trailing `Virtual::DeclEnd`.
    /// Caller must have verified `peek()` is `Virtual::DoBang`.
    pub(super) fn parse_do_bang(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::DO_BANG_EXPR));

        // `Virtual::DoBang` — rewrite of raw `Token::DoBang`. Same mechanics
        // as `Virtual::Then`: drain trivia to the virtual's start, emit the
        // raw text as `DO_BANG_TOK`, advance both cursors.
        if let Some((Ok(FilteredToken::Virtual(Virtual::DoBang)), do_span)) = self.peek().cloned() {
            self.drain_raw_up_to(do_span.start);
            debug_assert!(
                matches!(
                    self.raw_tokens.get(self.raw_pos),
                    Some((Ok(TriviaToken::Lexed(Token::DoBang)), s)) if *s == do_span,
                ),
                "Virtual::DoBang must be backed by a raw Token::DoBang at raw_pos with matching span"
            );
            self.emit_text(SyntaxKind::DO_BANG_TOK, do_span);
            self.raw_pos += 1;
            self.pos += 1;
        }

        // Body block: `Virtual::BlockBegin` … body … `Virtual::BlockEnd`,
        // handled exactly like an `if`/`then` body.
        let opened = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)),
        );
        if opened {
            self.bump_into(SyntaxKind::ERROR);
        }
        self.parse_if_body("do!", opened);

        // Trailing `Virtual::DeclEnd` closes the `do!` statement. Shared with
        // `while … do …`: a synthetic offside close is consumed as a zero-width
        // ERROR (raw stays put so a swallowed enclosing `}` still reaches
        // `bump_swallowed_closer`), while an explicit `done` terminator
        // (`do! f done`) is claimed as `DONE_TOK`.
        self.consume_block_decl_end();

        self.builder.finish_node();
    }

    /// `pars.fsy:4211 hardwhiteDoBinding %prec expr_let` — a `do e` statement
    /// (#light syntax), projecting to `SynExpr.Do(e, range)`. At module level
    /// FCS wraps it in `SynModuleDecl.Expr`, so this fires through the ordinary
    /// `EXPR_DECL` path; in a sequence/CE body it is one `Sequential` element.
    /// Emits `DO_EXPR > [DO_TOK, ERROR(BlockBegin), <body>, ERROR(BlockEnd),
    /// ERROR(DeclEnd)]`.
    ///
    /// Byte-for-byte the [`Self::parse_do_bang`] mechanics, only with the plain
    /// `do` keyword: LexFilter rewrites the raw `Token::Do` to `Virtual::Do`
    /// (raw still at `raw_pos`, same span — the `while`/`for` `do`-body relabel)
    /// and wraps the body in a SeqBlock, so emit the keyword text from the
    /// virtual, consume the `BlockBegin`/`BlockEnd` scaffolding as zero-width
    /// `ERROR` via [`Self::parse_if_body`], then the trailing `Virtual::DeclEnd`
    /// via [`Self::consume_block_decl_end`]. Caller must have verified `peek()`
    /// is `Virtual::Do`.
    pub(super) fn parse_do_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::DO_EXPR));

        // `Virtual::Do` — rewrite of raw `Token::Do`. Same mechanics as
        // `parse_do_bang`'s `DO_BANG_TOK`: drain trivia to the virtual's start,
        // emit the raw text as `DO_TOK`, advance both cursors.
        if let Some((Ok(FilteredToken::Virtual(Virtual::Do)), do_span)) = self.peek().cloned() {
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
        }

        // Body block: `Virtual::BlockBegin` … body … `Virtual::BlockEnd`,
        // handled exactly like an `if`/`then` body (a multi-statement body
        // wraps in `SEQUENTIAL_EXPR`).
        let opened = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)),
        );
        if opened {
            self.bump_into(SyntaxKind::ERROR);
        }
        self.parse_if_body("do", opened);

        // Trailing `Virtual::DeclEnd` closes the `do` statement — shared with
        // `do!` / `while … do …`: a synthetic offside close is consumed as a
        // zero-width ERROR, an explicit `done` terminator (`do f done`) claimed
        // as `DONE_TOK`.
        self.consume_block_decl_end();

        self.builder.finish_node();
    }

    /// Emit the keyword backing a CE-binder virtual (`Virtual::Binder` for
    /// `let!`/`use!`, `Virtual::AndBang` for `and!`) as `kind`, advancing both
    /// cursors — the same drain + `emit_text` + advance dance as
    /// `parse_do_bang`'s `DO_BANG_TOK`. The raw `Token::LetBang`/`UseBang`/
    /// `AndBang` still sits at `raw_pos` with the virtual's span, so the
    /// `let!`-vs-`use!`-vs-`and!` distinction is recoverable from the emitted
    /// token's text.
    fn emit_binder_keyword(&mut self, kind: SyntaxKind) {
        let span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("emit_binder_keyword invoked without a peeked binder virtual");
        self.drain_raw_up_to(span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::LetBang | Token::UseBang | Token::AndBang)), s))
                    if *s == span,
            ),
            "binder virtual must be backed by a raw let!/use!/and! at raw_pos with matching span"
        );
        self.emit_text(kind, span);
        self.raw_pos += 1;
        self.pos += 1;
    }

    /// Consume the current filtered token as a zero-width `ERROR` placeholder
    /// iff it is the virtual `v`, advancing only the filtered cursor (raw
    /// stays put, so a swallowed enclosing `}` still reaches
    /// [`Self::bump_swallowed_closer`]). Returns whether it fired. Used for the
    /// `Virtual::BlockEnd`/`DeclEnd` offside scaffolding around a binder.
    pub(super) fn eat_zero_width_virtual(&mut self, v: Virtual) -> bool {
        if matches!(self.peek(), Some((Ok(FilteredToken::Virtual(vv)), _)) if *vv == v) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Consume the offside scaffolding closing a binder's binding: the RHS
    /// `Virtual::BlockEnd` (`parse_binding` leaves the cursor before it), the
    /// explicit-`in` keyword if present, and the binder's `Virtual::DeclEnd`.
    ///
    /// The `in` reaches us two ways. In the block-let form (LexFilter's
    /// `Virtual::Let`/`Binder`) the IN arm folds it into a `Virtual::DeclEnd` at
    /// the `in`'s span and does *not* surface the raw `Token::In` in the filtered
    /// stream, so it is claimed from the raw stream (raw cursor only). In the
    /// *non-block* `let … in` operand form (a raw `Token::Let`/`Token::Use`), the
    /// `in` is a real *filtered* `Raw(In)` token, so it is consumed with
    /// `bump_into` (advancing both cursors) — otherwise the filtered cursor would
    /// stall on it and the body parse would see `in` instead of the body.
    fn close_binder_binding(&mut self) {
        self.eat_zero_width_virtual(Virtual::BlockEnd);
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::In)), _))) {
            self.bump_into(SyntaxKind::IN_TOK);
            self.eat_zero_width_virtual(Virtual::DeclEnd);
            return;
        }
        self.claim_swallowed_in();
        self.eat_zero_width_virtual(Virtual::DeclEnd);
    }

    /// If a LexFilter-swallowed `in` — a raw `Token::In` behind a binding's
    /// `Virtual::DeclEnd`, absent from the filtered stream — sits at the cursor,
    /// claim it as a clean `IN_TOK` and advance the raw cursor past it (leaving
    /// the filtered cursor for the caller to bump the `DeclEnd`). Returns whether
    /// it fired. Keeps the bare declaration-terminator `in` of a decl-flat
    /// `let … in⏎ <sibling>` (module or class body) from stranding as an
    /// "unsupported token In" ERROR. Shared by [`Self::close_binder_binding`] and
    /// the class-body item terminator.
    pub(super) fn claim_swallowed_in(&mut self) -> bool {
        let in_span = match self.next_non_trivia_raw_at_pos_with_span() {
            Some((Token::In, s)) => Some(s.clone()),
            _ => None,
        };
        if let Some(in_span) = in_span {
            self.drain_raw_up_to(in_span.start);
            self.emit_text(SyntaxKind::IN_TOK, in_span);
            self.raw_pos += 1;
            true
        } else {
            false
        }
    }

    /// `let! p = e [in|⏎] body` and `use! p = e …` — a computation-expression
    /// binder (`SynExpr.LetOrUse` with `IsBang = true`). LexFilter gives the
    /// binder the same `CtxtLetDecl` offside scaffolding as plain `let`
    /// (`Virtual::Binder`, then the binding RHS in a `BlockBegin…BlockEnd`
    /// block, then `DeclEnd`), and the body follows as the rest of the
    /// enclosing SeqBlock (after an offside `BlockSep`, or directly in the
    /// explicit-`in` form). `and!` followers (`Virtual::AndBang`) are collected
    /// as additional sibling `BINDING`s in the *same* node — one `LetOrUse`
    /// with several `Bindings` — matching FCS's applicative grouping. Emits
    /// `LET_OR_USE_EXPR > [BINDER_TOK, BINDING, (AND_BANG_TOK, BINDING)*,
    /// <scaffolding ERRORs>, <body-expr>]`.
    ///
    /// CE binders take no `inline`/`mutable` modifier (FCS's `OBINDER …`
    /// production omits them), so the bindings are parsed with
    /// `parse_binding_with_modifiers(false)`.
    ///
    /// Caller must have verified `peek()` is `Virtual::Binder`.
    pub(super) fn parse_let_or_use_bang(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LET_OR_USE_EXPR));

        // Head binder `let!`/`use!`, its binding, and the binding's close.
        self.emit_binder_keyword(SyntaxKind::BINDER_TOK);
        self.parse_binding_with_modifiers(false);
        self.close_binder_binding();

        // `and!` followers form one `LetOrUse` (`SynLetOrUse.Bindings`,
        // `IsRecursive = false`). Each is separated from the previous binding by
        // a `Virtual::BlockSep`; consume it, and if a `Virtual::AndBang`
        // follows, take another binding. That same `BlockSep` consumption is
        // *also* the body separator when the next token is not `and!` (the
        // offside form); the explicit-`in` form has no separating `BlockSep`.
        loop {
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _)),
            ) {
                self.bump_into(SyntaxKind::ERROR);
            }
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::AndBang)), _)),
            ) {
                self.emit_binder_keyword(SyntaxKind::AND_BANG_TOK);
                self.parse_binding_with_modifiers(false);
                self.close_binder_binding();
                continue;
            }
            break;
        }

        // Body. One or more statements project to a single expr or
        // `SEQUENTIAL_EXPR` via the shared offside-block gather (see
        // [`Self::parse_seq_block_body`]), the same pattern as `parse_fun_expr`
        // / `parse_if_body`. The separator before the body (offside form) was
        // already consumed by the binder loop above.
        self.parse_seq_block_body("expected computation-expression body after binder");

        self.builder.finish_node(); // LET_OR_USE_EXPR
    }

    /// Emit the `let`/`use` keyword backing a `Virtual::Let` as `LET_TOK`,
    /// advancing both cursors — the plain-`let` analogue of
    /// [`Self::emit_binder_keyword`]. The raw `Token::Let`/`Token::Use` still
    /// sits at `raw_pos` with the virtual's span (LexFilter's `OffsideLet`
    /// rewrite keeps the underlying raw), so the `let`-vs-`use` distinction is
    /// recoverable from the emitted token's text (matching
    /// [`crate::syntax::LetDecl::is_use`]). Returns the keyword span so the
    /// caller can anchor the non-`rec` `let … and …` diagnostic at it.
    fn emit_let_keyword(&mut self) -> std::ops::Range<usize> {
        let span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("emit_let_keyword invoked without a peeked let virtual");
        self.drain_raw_up_to(span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::Let | Token::Use)), s)) if *s == span,
            ),
            "Virtual::Let must be backed by a raw Token::Let / Token::Use at raw_pos with matching span"
        );
        self.emit_text(SyntaxKind::LET_TOK, span.clone());
        self.raw_pos += 1;
        self.pos += 1;
        span
    }

    /// Emit the shared `let`/`use` binding-group head into the currently open
    /// node: the keyword (`LET_TOK`), an optional `REC_TOK`, the first
    /// `BINDING`, and any `and`-chained `[AND_TOK BINDING]` followers, recording
    /// FCS error 576 (`parsLetAndForNonRecBindings`) for a non-recursive
    /// `and`-chain. Does *not* open/close a wrapper node, drain leading trivia,
    /// or consume the final binding's RHS `Virtual::BlockEnd` — the caller owns
    /// those. Shared by the module-level [`Self::parse_let_decl_at`], the module
    /// let-in dispatch [`Self::parse_module_let`], and the expression-level
    /// [`Self::parse_let_or_use_expr`], which all build the identical
    /// `[LET_TOK, REC_TOK?, BINDING, (AND_TOK BINDING)*]` prefix.
    ///
    /// Two keyword shapes reach here: LexFilter's `Virtual::Let` (the block
    /// form) needs [`Self::emit_let_keyword`]'s raw-realignment, while a raw
    /// `Token::Let`/`Token::Use` (a `;`-separated inline `let`, or a non-block
    /// `let … in` operand) is a plain `bump_into`.
    pub(super) fn parse_let_head_and_bindings(&mut self) {
        let let_span = if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::Let)), _))
        ) {
            self.emit_let_keyword()
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .expect("parse_let_head_and_bindings invoked without a let keyword");
            self.bump_into(SyntaxKind::LET_TOK);
            span
        };

        // Optional `rec` (FCS's `opt_rec`); LexFilter leaves `Token::Rec` raw.
        let is_rec = matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Rec)), _)));
        if is_rec {
            self.bump_into(SyntaxKind::REC_TOK);
        }

        // First binding, then any `and`-chained followers. LexFilter keeps
        // `CtxtLetDecl` open across an `and` whose column is ≥ the `let`'s, so
        // each prior binding's RHS `BlockEnd` (and any `BlockSep`) precedes the
        // raw `Token::And`; consume that run before the `AND_TOK`.
        self.parse_binding();
        let mut and_chained = false;
        while self.next_token_past_rhs_close_is(|t| matches!(t, FilteredToken::Raw(Token::And))) {
            and_chained = true;
            while matches!(
                self.peek(),
                Some((
                    Ok(FilteredToken::Virtual(
                        Virtual::BlockEnd | Virtual::BlockSep
                    )),
                    _,
                ))
            ) {
                self.bump_into(SyntaxKind::ERROR);
            }
            self.bump_into(SyntaxKind::AND_TOK);
            self.parse_binding();
        }

        // FCS error 576: a non-recursive `let … and …` group is a parse error
        // reported at the `let` keyword; the AST still holds both bindings
        // (purely a diagnostic), so the shape stays identical to the rec form.
        if and_chained && !is_rec {
            self.errors.push(ParseError {
                message: "non-recursive `let ... and ...` bindings are not allowed; \
                          use `let rec` or separate `let` declarations"
                    .to_string(),
                span: let_span,
            });
        }
    }

    /// Module/namespace-scope `let`/`use` dispatch — the impl-file loop's
    /// non-attributed `let` arm. Three source shapes share the `let x = e` head
    /// but diverge after the final binding's RHS `Virtual::BlockEnd`:
    ///  - no explicit `in` (`let x = e`) → `SynModuleDecl.Let` (flat).
    ///  - explicit `in` *not* directly followed by a body — a dedent to a
    ///    sibling declaration (`let a = 0 in⏎ let b = 1 in⏎ ()`, a
    ///    `Virtual::BlockSep` follows the swallowed `in`) or the enclosing scope
    ///    closing after it (`module M =⏎ let a = 0 in`, a `Virtual::BlockEnd` /
    ///    verbose `Token::End` follows) → still a flat `SynModuleDecl.Let`; the
    ///    `in` is a bare declaration terminator (FCS records
    ///    `SynBinding.trivia.InKeyword = None`). We claim it as a clean `IN_TOK`
    ///    so it does not strand as an "unsupported token In" ERROR.
    ///  - explicit `in` then a body directly (`let a = 0 in body`) →
    ///    `SynModuleDecl.Expr(SynExpr.LetOrUse)` — the `let … in` is an
    ///    *expression*, nesting if the body is itself a `let … in`.
    ///
    /// The three are indistinguishable until the binding's RHS is parsed (an
    /// `and`-chain or a nested RHS block pushes the terminal `BlockEnd`
    /// arbitrarily far), so the head+bindings are emitted at a checkpoint and
    /// the node kind is chosen retroactively. The expression form's tail mirrors
    /// [`Self::parse_let_or_use_expr`] (`close_binder_binding` +
    /// `parse_seq_block_body`), wrapped in `EXPR_DECL > LET_OR_USE_EXPR`.
    ///
    /// Caller must have verified `peek()` is `Virtual::Let` or (at body top
    /// level) a raw `Token::Let`/`Token::Use`.
    pub(super) fn parse_module_let(&mut self) {
        let let_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_module_let invoked without a peeked let token");
        // Leading trivia stays a sibling of the eventual decl node (mirror
        // `parse_module_decl` / `parse_let_decl_at`); take the checkpoint past
        // it so the retroactive wrapper covers only the decl's own tokens.
        self.drain_raw_up_to(let_span.start);
        let cp = self.builder.checkpoint();
        self.parse_let_head_and_bindings();
        // Classify from the binding terminator — the first filtered token past
        // the final binding's RHS-close `Virtual::BlockEnd` run (the cursor sits
        // at that `BlockEnd`; a recovered RHS leaves it elsewhere → `None` → a
        // plain flat `LET_DECL`, exactly as before let-in support). An explicit
        // `in` surfaces in one of two terminator shapes:
        //  - a `Virtual::DeclEnd` backed by a raw `Token::In` — the block-leading
        //    (`Virtual::Let`) form and the plain module `let`; LexFilter rewrote
        //    the `in` to a decl-end. (A `DeclEnd` *not* backed by a raw `In` is a
        //    plain no-`in` `let` — left to the loop.)
        //  - a raw `Token::In` directly (no `DeclEnd`) — a `let` reached as a raw
        //    keyword after a same-line separator (`open X; let a = 0 in body`),
        //    where the `in` is never layout-rewritten.
        let explicit_in_terminator = self
            .binding_terminator_index()
            .filter(|&term_idx| match self.filtered_tokens.get(term_idx) {
                Some((Ok(FilteredToken::Virtual(Virtual::DeclEnd)), _)) => matches!(
                    self.next_non_trivia_raw_at_pos_with_span(),
                    Some((Token::In, _))
                ),
                Some((Ok(FilteredToken::Raw(Token::In)), _)) => true,
                _ => false,
            });
        if let Some(term_idx) = explicit_in_terminator {
            // The expression form (`let x = e in body`) is the only shape where
            // a real expression atom sits *directly* after the `in`'s terminator,
            // with no intervening layout marker. Every other shape — a dedent to
            // a sibling decl (`Virtual::BlockSep`), the enclosing scope closing
            // (`Virtual::BlockEnd` for a module/`begin…end` offside block, a raw
            // `Token::End` for the verbose closer), or end of file — is the
            // decl-flat `SynModuleDecl.Let`. Gate on positive evidence of a body
            // (an expression starter at that offset) rather than blocklisting the
            // closers, so an unenumerated scope closer defaults to decl-flat. Use
            // the *full* `declExpr` starter set (`expr_start_at`, what
            // `parse_seq_block_body` accepts) — not the narrower infix-RHS
            // `is_expr_start_at`, which would miss `declExpr`-only body leaves
            // like `let a = 0 in do ()` or `let a = 0 in ..3`. (Both terminator
            // shapes are a single filtered token, so the body — if any — is at
            // `term_idx + 1` either way.)
            let body_follows = self.expr_start_at(term_idx + 1);
            if body_follows {
                // `let x = e in body` → SynModuleDecl.Expr(SynExpr.LetOrUse).
                // Consume the RHS close + `in` (+ `DeclEnd`), parse the body, then
                // wrap retroactively (mirrors `parse_let_or_use_expr`'s tail).
                // `close_binder_binding` claims either terminator shape.
                self.close_binder_binding();
                self.parse_seq_block_body("expected expression after `let … in`");
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::EXPR_DECL));
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LET_OR_USE_EXPR));
                self.builder.finish_node(); // LET_OR_USE_EXPR
                self.builder.finish_node(); // EXPR_DECL
                return;
            }
            // Decl-flat `let x = e in⏎ <sibling>`: a flat `SynModuleDecl.Let`.
            // Claim the swallowed `in` as a clean `IN_TOK` (+ the RHS-close
            // `BlockEnd`/`DeclEnd` as their usual zero-width ERRORs) so it does
            // not strand as an "unsupported token In" ERROR. FCS drops it
            // (`InKeyword = None`); the flat `Let` decl is already built.
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LET_DECL));
            self.builder.finish_node();
            self.close_binder_binding();
            return;
        }
        // Plain `let x = e` (no explicit `in`) or a recovered RHS: a flat
        // `SynModuleDecl.Let`, leaving the terminator `BlockEnd · DeclEnd` for
        // the loop (unchanged from the pre-let-in behaviour).
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LET_DECL));
        self.builder.finish_node();
    }

    /// The filtered index of the final binding's terminator — the first token
    /// right after the RHS-close `Virtual::BlockEnd` run — iff the cursor sits at
    /// that `BlockEnd` (where `parse_binding` leaves a well-formed binding);
    /// `None` if the RHS parse recovered mid-stream and the cursor is elsewhere.
    /// The terminator token itself is *not* inspected here — the caller
    /// classifies it (a `Virtual::DeclEnd`, possibly backed by a raw `Token::In`,
    /// for the block-leading form; a raw `Token::In` for the raw-keyword form; or
    /// anything else for a plain no-`in` `let`). Used by [`Self::parse_module_let`]
    /// to decide between the flat and let-in forms without assuming a particular
    /// terminator shape.
    fn binding_terminator_index(&self) -> Option<usize> {
        let mut i = self.pos;
        if !matches!(
            self.filtered_tokens.get(i),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
        ) {
            return None;
        }
        while matches!(
            self.filtered_tokens.get(i),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
        ) {
            i += 1;
        }
        Some(i)
    }

    /// Non-mutating lookahead from a module `let`/`Virtual::Let` at the cursor:
    /// `true` iff it is the inline let-in *expression* form — an explicit `in`
    /// with an expression body directly after it, i.e. what [`Self::parse_module_let`]
    /// would wrap as `EXPR_DECL > LET_OR_USE_EXPR` rather than a flat `LET_DECL`.
    ///
    /// [`Self::parse_module_let`] classifies *after* parsing the head+bindings
    /// (the cursor is then at the terminator), which is the natural path for a
    /// bare module `let`. The attributed dispatch, however, must decide *before*
    /// parsing whether to detach the attribute lists into their own
    /// `ATTRIBUTES_DECL` (FCS floats them off `[<A>] let x = e in body` →
    /// `[Attributes; Expr(LetOrUse)]`) — and rowan's builder cannot retroactively
    /// split the attributes from the let into sibling nodes. So this predicate
    /// finds the terminator by lookahead instead: scan forward tracking
    /// `BlockBegin`/`BlockEnd` depth (relative to the `let`), and at the first
    /// `Virtual::DeclEnd` / raw `Token::In` / `Virtual::BlockSep` seen back at
    /// depth 0 after the binding's RHS block, classify the same way
    /// `parse_module_let` does. An explicit `in` shows as a non-zero-width
    /// `DeclEnd` (it spans the `in`; a layout decl-end is zero-width) or a raw
    /// `Token::In`; a `BlockSep` (dedent to a sibling) or end of input is
    /// decl-flat. Conservative: any unrecognised shape returns `false` (attach),
    /// preserving the ordinary attributed-`let` behaviour.
    pub(super) fn module_let_is_inline_in_expr(&self) -> bool {
        if !matches!(
            self.filtered_tokens.get(self.pos),
            Some((
                Ok(FilteredToken::Virtual(Virtual::Let)
                    | FilteredToken::Raw(Token::Let | Token::Use)),
                _
            ))
        ) {
            return false;
        }
        let mut depth: i32 = 0;
        let mut saw_block = false;
        let mut i = self.pos + 1;
        while let Some((res, span)) = self.filtered_tokens.get(i) {
            match res {
                Ok(FilteredToken::Virtual(Virtual::BlockBegin)) => {
                    depth += 1;
                    saw_block = true;
                }
                Ok(FilteredToken::Virtual(Virtual::BlockEnd)) => depth -= 1,
                // The binding's RHS block has closed (back to the `let`'s own
                // level) and an explicit `in` terminator follows: the expression
                // form iff a real body starts right after it.
                Ok(FilteredToken::Virtual(Virtual::DeclEnd)) if depth <= 0 && saw_block => {
                    return span.start < span.end && self.expr_start_at(i + 1);
                }
                Ok(FilteredToken::Raw(Token::In)) if depth <= 0 && saw_block => {
                    return self.expr_start_at(i + 1);
                }
                // A dedent to a sibling declaration (or end of input) with no
                // intervening body — decl-flat, so the attribute attaches.
                Ok(FilteredToken::Virtual(Virtual::BlockSep)) if depth <= 0 && saw_block => {
                    return false;
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// `let p = e [in|⏎] body` and `use p = e …` in *expression* position —
    /// `SynExpr.LetOrUse` with `IsBang = false`. The plain-`let` sibling of
    /// [`Self::parse_let_or_use_bang`], sharing the `LET_OR_USE_EXPR` node and
    /// the `close_binder_binding` / `parse_seq_block_body` machinery; the token
    /// shape mirrors the module-level [`crate::syntax::LetDecl`]
    /// (`[LET_TOK, REC_TOK?, BINDING, (AND_TOK BINDING)*, …]`) plus the nested
    /// body, so the normaliser tells the two apart by the head token
    /// (`LET_TOK` here vs `BINDER_TOK` for the bang form).
    ///
    /// LexFilter gives the offside `let` the same `CtxtLetDecl` scaffolding as a
    /// module-level `let` (the binding RHS in a `BlockBegin…BlockEnd` block, a
    /// `DeclEnd`, then the body after an offside `BlockSep` — or, in the
    /// explicit-`in` form, no `BlockSep` and a raw `Token::In`
    /// `close_binder_binding` claims). The body goes through
    /// [`Self::parse_seq_block_body`], so it may itself be another `let`
    /// (nesting, `LetOrUse([a], LetOrUse([b], …))`) or a multi-statement
    /// `Sequential`.
    ///
    /// Unlike the bang binder, a plain `let` takes `inline`/`mutable` modifiers
    /// and `rec`/`and` chains, so bindings parse with the default
    /// `parse_binding` (modifiers on) and the `and`-chain reuses the
    /// module-level [`Self::next_token_past_rhs_close_is`] pattern.
    ///
    /// Two keyword shapes reach here. A *block-leading* `let`/`use` (a function/
    /// `let`/`fun`/`if`/`match` body, a paren body) is LexFilter's `Virtual::Let`.
    /// A *non-block* `let … in` — one that is a mid-expression operand (an infix
    /// RHS `a && let x = e in b`, a tuple element, a `lazy`/`assert`/`fixed`
    /// operand) — surfaces instead as a *raw* `Token::Let` with an explicit
    /// `Raw(In)` (or, the offside-body form, the body directly after the
    /// binding-RHS `BlockEnd`). Both forms share everything below: the binding's
    /// RHS still gets a `BlockBegin…BlockEnd` block, [`Self::close_binder_binding`]
    /// claims the explicit `in` (and tolerates its absence), and the body goes
    /// through [`Self::parse_seq_block_body`]. Only the head-keyword emission
    /// differs — a virtual needs [`Self::emit_let_keyword`]'s raw-realignment, a
    /// raw keyword is a plain `bump_into`. (Only the block form reaches here with
    /// `use`; a non-block raw `Token::Use` is *not* dispatched — see
    /// [`Self::parse_minus_expr`]'s note on FCS's `use`→`Let` relabel.)
    ///
    /// Caller must have verified `peek()` is `Virtual::Let` or a raw `Token::Let`.
    pub(super) fn parse_let_or_use_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LET_OR_USE_EXPR));

        // Head keyword, optional `rec`, first binding, and any `and`-chain (with
        // FCS error 576 for a non-recursive chain). Shared with the module-level
        // `parse_let_decl_at` / `parse_module_let`.
        self.parse_let_head_and_bindings();

        // Close the final binding (RHS `BlockEnd` + optional `in` + `DeclEnd`),
        // then the body-separator `BlockSep` (offside form only — the
        // explicit-`in` form has none), then the body.
        self.close_binder_binding();
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
        self.parse_seq_block_body("expected expression after `let` binding");

        self.builder.finish_node(); // LET_OR_USE_EXPR
    }

    /// `pars.fsy:5557 LBRACE braceExprBody RBRACE` — a brace expression. FCS
    /// overloads `{ … }` across record, object, and computation expressions
    /// (`braceExprBody`, `pars.fsy:5580`); this dispatches between them.
    ///
    /// Disambiguation (`braceExprBody = recdExpr | objExpr | computationExpr`):
    /// a leading longident followed by `=` (field assignment) or `with`
    /// (copy-update) is a record ([`Self::parse_record_body`]); everything else
    /// is a computation expression. Object expressions (`{ new T … }`) are
    /// deferred (they need member syntax), so they currently fall into the
    /// computation-expression arm. The decision is pure lookahead
    /// ([`Self::classify_brace_body`]), so the node kind is chosen before the
    /// `{` is consumed.
    ///
    /// The closing `}` is swallowed by LexFilter (absent from the filtered
    /// stream, like the `)` of a paren expression), so it is recovered from the
    /// raw stream by [`Self::bump_swallowed_closer`]. The builder-application
    /// form `seq { e }` is produced by the surrounding `parse_app_expr`
    /// juxtaposition; a bare `{ e }` stands alone.
    pub(super) fn parse_brace_expr(&mut self) {
        // Depth-guarded: the atom-level entry for every `{ … }` expression
        // (record / computation / object / appExpr-headed). A nested computation
        // expression recurses `parse_brace_expr → parse_app_head_brace →
        // parse_app_expr → (brace arg) parse_brace_expr` — a cycle that never
        // passes through `parse_pratt_expr`, so this is a distinct recursion
        // chokepoint that needs its own guard. The recursion re-enters the
        // public wrapper (via `parse_atomic_expr`), so each brace level counts.
        self.with_depth(Self::parse_brace_expr_inner);
    }

    fn parse_brace_expr_inner(&mut self) {
        // A `new`-headed brace is either an object expression `{ new T with
        // member … }` (FCS's `objExpr`) or a computation expression wrapping a
        // bare construction (`{ new T(args) }`). The two share the `new T(args)`
        // base call but only differ on what follows it (a `with`/interface block
        // vs. the closing `}`), which the base-call parser is the cheapest way to
        // discover (the closing `)` is LexFilter-swallowed, so a lookahead scan
        // cannot find where the args end). Route it through the dedicated
        // checkpoint-based handler.
        if self.peek_brace_body_is_new_head() {
            self.parse_obj_or_computation_brace();
            return;
        }
        let body = self.classify_brace_body();
        // An `appExpr`-headed brace can't pick its node kind by lookahead (the
        // copy source's closers are swallowed), so its `start_node` is deferred
        // to a checkpoint inside the dedicated handler.
        if matches!(body, BraceBody::AppExprHead) {
            self.parse_app_head_brace();
            return;
        }
        let node_kind = match body {
            BraceBody::Record { .. } => SyntaxKind::RECORD_EXPR,
            BraceBody::Computation => SyntaxKind::COMPUTATION_EXPR,
            // Routed above to `parse_app_head_brace` (deferred node kind).
            BraceBody::AppExprHead => unreachable!("AppExprHead handled before this match"),
        };
        self.builder.start_node(FSharpLang::kind_to_raw(node_kind));
        self.bump_into(SyntaxKind::LBRACE_TOK);
        // Drain trivia between `{` and the body so it attaches to the brace node
        // rather than the inner expr's — symmetric to `parse_paren_expr`. Drain
        // up to the next significant *raw* token: the first body token for a
        // non-empty brace, or the swallowed `}` for an empty record (`{ }`).
        // (Draining to the next *filtered* token would over-run the `}` for an
        // empty record, since the next filtered token sits past it.)
        if let Some((_, raw_span)) = self.next_non_trivia_raw_at_pos_with_span() {
            self.drain_raw_up_to(raw_span.start);
        }
        match body {
            BraceBody::Record { copy_update } => self.parse_record_body(copy_update),
            BraceBody::Computation => {
                // The CE body is a statement *sequence* — FCS's
                // `SynExpr.ComputationExpr` wraps the `computationExpr`
                // production, which is a bare `sequentialExpr` (**not** a
                // `typedSequentialExpr`), so `seq { yield 1; yield 2 }` is
                // `ComputationExpr(Sequential …)`. Use `parse_seq_block_elements`
                // (the non-typed variant): a bare trailing annotation
                // `seq { 1 : int }` is an FCS error (the `:` must be
                // parenthesised), so the typed-sequential hook must stay off here
                // or we would accept input FCS rejects. `;`-separated (single
                // line, raw `Semi`) and offside (`OBLOCKSEP`, multi-line)
                // separators are both handled, and it stops at the swallowed `}`
                // (`at_swallowed_seq_closer`), emitting its own "missing first"
                // error for an empty body.
                self.parse_seq_block_elements("expected expression inside `{ }`");
            }
            // Routed earlier to `parse_app_head_brace` (deferred node kind).
            BraceBody::AppExprHead => unreachable!("AppExprHead handled before this match"),
        }
        let closer_context = match body {
            BraceBody::Record { .. } => "record expression",
            BraceBody::Computation => "computation expression",
            BraceBody::AppExprHead => unreachable!("AppExprHead handled before this match"),
        };
        self.bump_swallowed_closer(
            SyntaxKind::RBRACE_TOK,
            |t| matches!(t, Token::RBrace),
            "}",
            closer_context,
        );
        self.builder.finish_node();
    }

    /// `true` iff the `{ … }` body's first filtered token (after the `{` at
    /// `self.pos`) is the raw `new` keyword — the head of either an object
    /// expression (`{ new T with member … }`) or a computation expression
    /// wrapping a bare construction (`{ new T(args) }`). The cursor is at the
    /// `{`. Used by [`Self::parse_brace_expr`] to divert to the dedicated
    /// [`Self::parse_obj_or_computation_brace`] handler.
    fn peek_brace_body_is_new_head(&self) -> bool {
        matches!(
            self.filtered_tokens.get(self.pos + 1),
            Some((Ok(FilteredToken::Raw(Token::New)), _))
        )
    }

    /// Parse a `new`-headed brace expression — either an object expression
    /// ([`SyntaxKind::OBJ_EXPR`], FCS's `objExpr` `pars.fsy:5828`) or a
    /// computation expression wrapping a bare construction
    /// ([`SyntaxKind::COMPUTATION_EXPR`], `{ new T(args) }`).
    ///
    /// The two forms share the `new T(args)` base call (FCS's `objExprBaseCall`,
    /// which is the same `NEW atomType opt_HIGH_PRECEDENCE_APP atomicExprAfterType`
    /// as a plain `SynExpr.New`) but diverge on what follows it: a `with`
    /// bindings/members block or an `interface` impl makes it an object
    /// expression, while the closing `}` makes it a computation expression
    /// whose single statement is the construction (`{ new T(1, 2) }` is
    /// `ComputationExpr(New(T, …))`, *not* an `ObjExpr` — FCS only reaches
    /// `objExpr` via the bindings/interfaces or the bare no-arg `NEW atomType`
    /// alternative). Because the construction's closing `)` is LexFilter-
    /// swallowed, a pre-scan cannot locate the end of the args, so the base call
    /// is parsed *for real* under a checkpoint and the node kind is chosen
    /// afterwards via [`rowan::GreenNodeBuilder`]'s `start_node_at`.
    ///
    /// **Supported:** the `with member …` member form (the reported bug,
    /// `{ new IDisposable with member x.Dispose () = () }`), the optional base
    /// alias `as base` (`baseSpec`), the trailing `interface I with member …`
    /// implementations (`extraImpls`, via the loop below), the interface-only
    /// form `{ new T() interface I with … }` (FCS's `objExpr` alt 2, when the
    /// interface immediately follows the base call), the bare no-parens form
    /// `{ new T }` (FCS's `objExpr` alt `NEW atomType`, distinguished from the
    /// parenthesised computation `{ new T() }` by the absence of constructor
    /// args — see `is_bare_new` below), and any computation expression.
    /// **Deferred** (later stage, reject without corruption): the
    /// `with`-`localBindings` form (`{ new T() with X = e }`, which arrives as
    /// `OWITH`/`OEND` rather than a raw `with`) — it falls through to the
    /// computation arm, carrying `parse_new_expr`'s missing-argument note.
    fn parse_obj_or_computation_brace(&mut self) {
        // Checkpoint before the `{` so the chosen node (object or computation)
        // can be wrapped around the whole brace once the form is known.
        let outer_cp = self.builder.checkpoint();
        self.bump_into(SyntaxKind::LBRACE_TOK);
        // Drain trivia between `{` and `new` so it attaches to the brace node,
        // symmetric to `parse_brace_expr`.
        if let Some((_, raw_span)) = self.next_non_trivia_raw_at_pos_with_span() {
            self.drain_raw_up_to(raw_span.start);
        }
        // Stamp the head `new`'s span (the cursor is at it now) so the base
        // call's `parse_new_expr` can recognise itself as this brace's head and
        // flag the bare `{ new T }` form. Saved/restored for nesting (an inner
        // `new`-headed brace in a constructor argument runs this same handler).
        let saved_base_new = self.obj_brace_base_new.take();
        let saved_base_no_arg = std::mem::replace(&mut self.obj_brace_base_no_arg, false);
        self.obj_brace_base_new = self.peek().map(|(_, s)| s.clone());
        // Checkpoint at the body's first expression, so the computation arm can
        // wrap a multi-statement body (`{ new T(); yield 1 }`) in a
        // `SEQUENTIAL_EXPR` from here (the base call is the sequence's first
        // element). Unused on the object-expression path.
        let body_cp = self.builder.checkpoint();
        // Parse the base call (`new T(args)`) — or, in the computation case, the
        // full expression of which it is the head (`{ new T(x) + 1 }`). This
        // produces a `NEW_EXPR` (or a larger expression wrapping it). When the
        // type is followed directly by a `with`/interface block (no args),
        // `parse_new_expr` suppresses its missing-argument error.
        self.parse_expr();
        // The bare no-parens form `{ new T }` (FCS's `objExpr` alt `NEW
        // atomType`): set iff the head `new` had no constructor argument and the
        // brace closed directly after the type. Read it before restoring the
        // saved nesting state.
        let is_bare_new = self.obj_brace_base_no_arg;
        self.obj_brace_base_new = saved_base_new;
        self.obj_brace_base_no_arg = saved_base_no_arg;
        // An object expression iff a base alias (`as base`) or a `with`
        // bindings/members block follows the base call *in this brace*. The base
        // alias is FCS's `baseSpec` (the `Ident option` half of `argOptions`),
        // which occurs *only* in an object expression — `as` is never an
        // expression operator, so `parse_expr` always stops before it. The member
        // form's `with` is the raw `Token::With` (the `WithAsAugment` context,
        // identical to a `type T with member …` augmentation).
        //
        // Discriminate using **both** token streams — they answer two distinct
        // questions, and either alone misclassifies a known case:
        //
        //  * The **raw** stream (`next_non_trivia_raw_at_pos`) is the nesting
        //    guard. A genuine marker for *this* brace is the next *significant
        //    raw* token after the base call. The filtered `peek()` skips
        //    LexFilter-swallowed closers (`}`/`)`), so for a `new`-headed brace
        //    nested in an enclosing object expression's constructor argument —
        //    the inner `seq { new Bar() }` in `{ new Foo(seq { new Bar() }) with
        //    … }` — `peek()` alone would see the *outer* `with` and steal its
        //    member block. When this brace is already closed, the next raw token
        //    is its swallowed `}` (or an enclosing `)`), so the raw check fails.
        //
        //  * The **filtered** `peek()` distinguishes the two `with`-forms, which
        //    share the *same* underlying raw `Token::With`:
        //      - the **member** form `{ new T with member … }`, whose `with` is a
        //        raw `Token::With` (`WithAsAugment`, identical to a `type T with
        //        member …` augmentation), and
        //      - the **value-binding** form `{ new T() with X = e }`, whose `with`
        //        is `Virtual::With` (`OWITH`, the `WithAsLet` context — FCS's
        //        `objExprBindings: OWITH localBindings OEND`).
        //    The raw stream alone cannot tell them apart, so the filtered token
        //    selects which emission (the member offside block vs. the value
        //    `localBindings`) runs below.
        //
        // In a real single-level member object expression the marker is the next
        // token in *both* streams, so the `bump_into`/`parse_base_spec` emission
        // below (which reads `peek()`) lands on the right token.
        let next_raw = self.next_non_trivia_raw_at_pos();
        let is_member_with = matches!(next_raw, Some(Token::With))
            && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::With)), _)));
        // The value-binding form's `with` is the `Virtual::With` (`OWITH`) relabel
        // backed by the same raw `Token::With` (the raw stream is the nesting
        // guard, as for the member form).
        let is_value_with = matches!(next_raw, Some(Token::With))
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::With)), _))
            );
        let has_base_spec = matches!(next_raw, Some(Token::As))
            && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::As)), _)));
        // The interface-only form `{ new T() interface I with … }` (FCS's
        // `objExpr` alt 2) has no `with`/`as` — the base call is directly
        // followed by an `OINTERFACE_MEMBER`. (The `with member …` form, by
        // contrast, reaches its interfaces only *after* the member block, via the
        // loop below.) The bare form `{ new T }` (FCS's `objExpr` alt `NEW
        // atomType`) has none of those followers — `is_bare_new` was set by the
        // base call's `parse_new_expr` (see above); its tail below is the same
        // no-member close as the computation arm.
        let is_obj_expr = is_member_with
            || is_value_with
            || has_base_spec
            || self.peek_obj_expr_interface_only()
            || is_bare_new;
        if is_obj_expr {
            self.builder
                .start_node_at(outer_cp, FSharpLang::kind_to_raw(SyntaxKind::OBJ_EXPR));
            // The base alias `as <ident>` (`baseSpec`), if present, before the
            // `with`. Shared with `inherit … as base` — emitted as `AS_TOK` (+
            // alias `IDENT_TOK`) and elided by the normaliser (the `Ident option`
            // in `argOptions`).
            if has_base_spec {
                self.parse_base_spec();
            }
            // The `with member …` block: emit the `with` and reuse the shared
            // offside member loop. Its members become direct children of the
            // open `OBJ_EXPR` node (FCS's `members` slot). A bare `as base` with
            // no `with` (rare/malformed) just closes here.
            //
            // Discriminated on **both** streams (re-evaluated here, since
            // `parse_base_spec` above may have advanced past an `as base` to land
            // on the `with`): the **raw** stream is the nesting guard, and the
            // **filtered** `peek()` confirms the raw form. Without the raw guard,
            // a *bare* object expression nested directly before this brace's `with`
            // — the inner `{ new Bar }` in `{ new Foo({ new Bar }) with member … }`
            // — would have `peek()` surface *this* (outer) `with` past the inner's
            // swallowed `}`/`)`, and the inner brace (now itself an `OBJ_EXPR` via
            // `is_bare_new`) would steal the outer member block. The raw stream's
            // next significant token there is the inner `}`, so the guard fails and
            // the `with` is left to this outer brace.
            if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::With))
                && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::With)), _)))
            {
                self.bump_into(SyntaxKind::WITH_TOK);
                // An object expression's member block closes with the brace `}`
                // (a synthetic `OEND` with no raw `end`), never an explicit `end`
                // keyword; pass `false` so an empty block does not try to claim
                // one. The real-`Token::End` guard leaves the brace `OEND` anyway.
                self.parse_with_augmentation_members(false, false);
            } else if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::With))
                && matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::With)), _))
                )
            {
                // The value-binding `with X = e [and …]` block (FCS's
                // `objExprBindings: OWITH localBindings OEND`). The `with` is the
                // `Virtual::With` (`OWITH`) relabel; the bindings become `BINDING`
                // children of the open `OBJ_EXPR` (FCS's `bindings` slot, distinct
                // from the `MEMBER_DEFN` members and the `INTERFACE_IMPL`
                // extra-impls). Guarded by the same raw-stream nesting check as the
                // member form so a nested brace's swallowed `with` is not stolen.
                self.parse_obj_expr_value_bindings();
            }
            // Extra interface implementations (`opt_objExprInterfaces`, FCS's
            // `objExpr` alt 1 tail): `… with member … interface I with member …
            // interface J …`. Each is an `objExprInterface` → a
            // `SynInterfaceImpl`, the *same* `INTERFACE_IMPL` node the
            // type-definition interface member (9.11b) produces, so reuse
            // `parse_interface_member`. They become `INTERFACE_IMPL` children of
            // the open `OBJ_EXPR` (FCS's `extraImpls` slot, distinct from the
            // `MEMBER_DEFN` member children in `members`). The `with` member block
            // closes with FCS's `opt_declEnd` (drained by
            // `parse_with_augmentation_members`); each interface then follows after
            // an `opt_OBLOCKSEP` (and self-terminates the same way). Only drain the
            // separator virtuals when an interface actually follows — otherwise
            // they are this brace's close (left for `bump_swallowed_closer` and the
            // enclosing loop), so draining them would steal an enclosing scope's
            // closers (the Stage A sibling-decl discipline).
            while self.peek_obj_expr_interface_follows() {
                self.drain_close_virtuals_before_interface();
                self.parse_interface_member(false);
            }
            // `parse_with_augmentation_members` drains the member block's own
            // close virtuals (its `OBLOCKEND` + the `with`'s `ODECLEND`), leaving
            // any enclosing-scope separator for the caller — exactly as the type
            // augmentation does. The swallowed `}` is recovered from the raw
            // stream below; the enclosing `let`/module loop consumes the residual
            // virtuals. (Draining them here would steal the enclosing block's
            // close and mis-nest a following sibling declaration.)
            self.bump_swallowed_closer(
                SyntaxKind::RBRACE_TOK,
                |t| matches!(t, Token::RBrace),
                "}",
                "object expression",
            );
            self.builder.finish_node();
        } else {
            // A computation expression. The base call parsed above is the body's
            // first statement; gather any further `;`/offside-separated statements
            // (`{ new T(); yield 1 }`) into a `SEQUENTIAL_EXPR` from `body_cp`,
            // reusing the shared sequence discipline. `allow_typed = false`: the
            // `computationExpr` body is a bare `sequentialExpr` (a trailing `:`
            // must be parenthesised), matching the generic CE brace path.
            self.finish_seq_block(body_cp, 1, false);
            self.builder.start_node_at(
                outer_cp,
                FSharpLang::kind_to_raw(SyntaxKind::COMPUTATION_EXPR),
            );
            self.bump_swallowed_closer(
                SyntaxKind::RBRACE_TOK,
                |t| matches!(t, Token::RBrace),
                "}",
                "computation expression",
            );
            self.builder.finish_node();
        }
    }

    /// Whether an object-expression extra interface (`Virtual::InterfaceMember`,
    /// FCS's `OINTERFACE_MEMBER`) follows the current item *inside this brace* —
    /// the loop guard for [`Self::parse_obj_or_computation_brace`]'s
    /// `extraImpls`. A pure lookahead (no consumption): the loop calls this
    /// *before* draining the separators, so a trailing close virtual that is the
    /// brace's own (no interface after it) is left untouched for
    /// `bump_swallowed_closer` and the enclosing loop.
    ///
    /// Discriminated on **both** streams (like the brace-form selection): the
    /// **raw** stream is the nesting guard — the interface's `interface` keyword
    /// (a real raw token behind `OINTERFACE_MEMBER`) must be the next significant
    /// raw token, i.e. it precedes this brace's LexFilter-swallowed `}`. The
    /// filtered `peek()` skips swallowed `}`/`)` closers, so for an object
    /// expression nested in a type member body followed by the *type's* own
    /// `interface …` (`type T = member _.M = { new IFoo with … }⏎ interface I …`),
    /// the filtered stream past the swallowed `}` would surface the *outer*
    /// interface and the loop would steal it; when this brace is closed the next
    /// raw token is its `}`, not `interface`, so the raw check fails. The
    /// **filtered** scan (skipping close/separator virtuals) then confirms the
    /// `OINTERFACE_MEMBER` so the drain + `parse_interface_member` land correctly.
    fn peek_obj_expr_interface_follows(&self) -> bool {
        if !matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Interface)) {
            return false;
        }
        let mut i = self.pos;
        loop {
            match self.filtered_tokens.get(i) {
                Some((
                    Ok(FilteredToken::Virtual(
                        Virtual::BlockEnd | Virtual::DeclEnd | Virtual::BlockSep,
                    )),
                    _,
                )) => i += 1,
                Some((Ok(FilteredToken::Virtual(Virtual::InterfaceMember)), _)) => return true,
                _ => return false,
            }
        }
    }

    /// Whether the base call is *directly* followed by an object-expression
    /// interface (`Virtual::InterfaceMember`) — FCS's `objExpr` alt 2
    /// (`objExprBaseCall opt_OBLOCKSEP objExprInterfaces`), the interface-only
    /// form `{ new T() interface I with … }` (no preceding `with member` block).
    /// The brace-form selector in [`Self::parse_obj_or_computation_brace`] uses
    /// this as a third object-expression trigger beside `with`/`as`.
    ///
    /// FCS accepts the interface-only form **only** when the interface
    /// immediately follows the base call with no intervening `OBLOCKSEP` (the
    /// interface on the same line, or indented deeper than the brace); an offside
    /// interface on a separate line at-or-left of the brace column leaves an
    /// `OBLOCKSEP` before the `OINTERFACE_MEMBER` and is FCS's FS0010 error. So
    /// gate on the cursor sitting **directly** at the `OINTERFACE_MEMBER` (no
    /// virtual skip — contrast [`Self::peek_obj_expr_interface_follows`], which
    /// skips the member block's closes): a preceding `OBLOCKSEP` fails this and
    /// falls to the computation arm, which errors like FCS. The raw guard (next
    /// raw token is the `interface` keyword) keeps a nested brace's swallowed `}`
    /// from being mistaken for this brace's interface.
    fn peek_obj_expr_interface_only(&self) -> bool {
        matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Interface))
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::InterfaceMember)), _))
            )
    }

    /// Drain the close/separator virtuals (`BlockEnd`/`DeclEnd`/`BlockSep`) that
    /// sit between an object expression's preceding item and the next
    /// `Virtual::InterfaceMember`, each as a zero-width `ERROR` (advancing only
    /// the filtered cursor — the raw trivia is captured by the interface's own
    /// leading-trivia drain in [`Self::parse_interface_member`]). The caller has
    /// verified an interface follows ([`Self::peek_obj_expr_interface_follows`]),
    /// so this stops *at* the `InterfaceMember` and never over-runs into the
    /// brace's close.
    fn drain_close_virtuals_before_interface(&mut self) {
        while matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(
                    Virtual::BlockEnd | Virtual::DeclEnd | Virtual::BlockSep
                )),
                _
            ))
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }
    }

    /// Parse an anonymous-record expression `{| F = e; … |}`
    /// ([`SyntaxKind::ANON_RECD_EXPR`], FCS's `braceBarExpr` → `SynExpr.AnonRecd`).
    /// With `is_struct`, parses the `struct {| … |}` form (cursor at the
    /// `struct` keyword, emitted as a leading `STRUCT_TOK`); otherwise the
    /// cursor is at the `{|` ([`Token::LBraceBar`]). Mirrors
    /// [`Self::parse_brace_expr`]'s record arm — the field list is the same
    /// `recdExprCore` grammar (`pars.fsy:5917`), so [`Self::parse_record_field`]
    /// and the separator handling are reused. Two differences from a record:
    /// the closer `|}` ([`Token::BarRBrace`]) is a *real* filtered token (not
    /// swallowed like `}`), bumped directly; and there is no `baseInfo`.
    pub(super) fn parse_anon_recd_expr(&mut self, is_struct: bool) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ANON_RECD_EXPR));
        if is_struct {
            // `struct` keyword, then trivia up to the `{|`.
            self.bump_into(SyntaxKind::STRUCT_TOK);
            if let Some((_, raw_span)) = self.next_non_trivia_raw_at_pos_with_span() {
                self.drain_raw_up_to(raw_span.start);
            }
        }
        let copy_update = self.peek_anon_recd_is_copy_update();
        self.bump_into(SyntaxKind::LBRACE_BAR_TOK);
        // Drain trivia between `{|` and the body so it attaches to the
        // anon-record node, symmetric to `parse_brace_expr`.
        if let Some((_, raw_span)) = self.next_non_trivia_raw_at_pos_with_span() {
            self.drain_raw_up_to(raw_span.start);
        }
        self.parse_anon_recd_body(copy_update);
        // The `|}` closer is a real filtered token (unlike the record `}`), so
        // bump it directly rather than via `bump_swallowed_closer`.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::BarRBrace)), _))
        ) {
            self.bump_into(SyntaxKind::BAR_RBRACE_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `|}` to close the anonymous record".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// Parse a list `[ … ]` or array `[| … |]` *expression*
    /// ([`SyntaxKind::ARRAY_OR_LIST_EXPR`]) — FCS's `listExpr` (`pars.fsy:5298`)
    /// / `arrayExpr` (`:5450`), an `atomicExprAfterType` alternative. FCS splits
    /// the result by emptiness: an empty `[]` / `[||]` is
    /// `SynExpr.ArrayOrList(isArray, [], _)` (no body), while a non-empty
    /// bracket is `SynExpr.ArrayOrListComputed(isArray, body, _)` whose `body`
    /// is the single `sequentialExpr`. We mirror that with one node whose
    /// body-child is present iff non-empty.
    ///
    /// The body is the shared offside/`;` sequence gatherer
    /// ([`Self::parse_seq_block_body`], FCS's `sequentialExpr`): two-or-more
    /// `;`/`OBLOCKSEP`-separated elements wrap in one `SEQUENTIAL_EXPR`, a
    /// single element stays bare. The element separator is `;`, **not** `,`:
    /// `[a, b]` is a one-element list whose element is the tuple `(a, b)` (the
    /// `,` is absorbed by [`Self::parse_expr`]'s tuple layer), while `[a; b]`
    /// is two elements.
    ///
    /// The openers `[` / `[|` and closers `]` / `|]` are all real filtered
    /// tokens (the lex-filter does *not* swallow them, unlike `)` / `}`), so the
    /// `peek()`-based bumps are correct with no `bump_swallowed_*` dance. Caller
    /// ([`Self::parse_atomic_expr_head`]) has verified the cursor is at `[` or
    /// `[|`; any other token is `unreachable!`.
    pub(super) fn parse_array_or_list_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ARRAY_OR_LIST_EXPR));
        let is_array = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::LBrack)), _)) => {
                self.bump_into(SyntaxKind::LBRACK_TOK);
                false
            }
            Some((Ok(FilteredToken::Raw(Token::LBrackBar)), _)) => {
                self.bump_into(SyntaxKind::LBRACK_BAR_TOK);
                true
            }
            other => {
                unreachable!("parse_array_or_list_expr called without `[`/`[|`: {other:?}")
            }
        };

        // Drain trivia between the opener and the body so it attaches to this
        // node rather than the body expr's — symmetric to `parse_brace_expr`.
        // Drain up to the next significant *raw* token: the first body token
        // for a non-empty bracket, or the closer for an empty `[]` / `[||]`.
        if let Some((_, raw_span)) = self.next_non_trivia_raw_at_pos_with_span() {
            self.drain_raw_up_to(raw_span.start);
        }

        // `[` / `[|` close on different tokens; the closure shares one predicate
        // between the empty-body gate and the final bump.
        let at_close = |p: &Self| {
            if is_array {
                matches!(
                    p.peek(),
                    Some((Ok(FilteredToken::Raw(Token::BarRBrack)), _))
                )
            } else {
                matches!(p.peek(), Some((Ok(FilteredToken::Raw(Token::RBrack)), _)))
            }
        };

        // Empty `[]` / `[||]` is valid (FCS's `ArrayOrList(_, [], _)`) — only
        // parse a body when the cursor isn't already at the closer, so the
        // gatherer never reports a spurious "missing first element". List/array
        // elements are FCS's `sequentialExpr` (not `typedSequentialExpr`), so a
        // bare `[1 : int]` is rejected — use the no-annotation gatherer.
        if !at_close(self) {
            self.parse_seq_block_elements("expected expression in list or array expression");
        }

        if at_close(self) {
            self.bump_into(if is_array {
                SyntaxKind::BAR_RBRACK_TOK
            } else {
                SyntaxKind::RBRACK_TOK
            });
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: format!(
                    "expected `{}` to close the {} expression",
                    if is_array { "|]" } else { "]" },
                    if is_array { "array" } else { "list" },
                ),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// Parse a struct-tuple expression `struct (e1, e2, …)`
    /// ([`SyntaxKind::TUPLE_EXPR`] carrying a leading `STRUCT_TOK`, FCS's
    /// `STRUCT LPAREN tupleExpr rparen` → `SynExpr.Tuple(isStruct = true)`,
    /// `pars.fsy:5314`). The cursor is at the `struct` keyword. Unlike a regular
    /// `(1, 2)` (which is `Paren(Tuple(false, …))`), the parens belong to *this*
    /// node and there is no `Paren` wrapper — so the elements/commas and the
    /// `STRUCT_TOK` / parens sit directly under one `TUPLE_EXPR`, and
    /// [`crate::syntax::TupleExpr::is_struct`] reads the `STRUCT_TOK`. FCS
    /// requires ≥2 elements (`struct (1)` is a parse error), so a missing comma
    /// after the first element is reported.
    fn parse_struct_tuple_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TUPLE_EXPR));
        self.bump_into(SyntaxKind::STRUCT_TOK);
        // The opening `(` is a real filtered token (only the closing `)` is
        // swallowed); `bump_into` drains the `struct`/`(` trivia before it.
        self.bump_into(SyntaxKind::LPAREN_TOK);
        if let Some((_, next_span)) = self.peek() {
            let start = next_span.start;
            self.drain_raw_up_to(start);
        }

        // First element (a full expression below the tuple comma — the
        // range level, so `struct (1..3, 4)` parses like the ordinary tuple).
        if self.peek_is_expr_start() {
            self.parse_range_expr();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected an expression inside `struct (…)`".to_string(),
                span,
            });
        }

        // FCS's struct tuple needs ≥2 elements; a missing comma here (e.g.
        // `struct (1)`) is the "Unexpected symbol ')'" parse error.
        if !self.at_tuple_continuation() {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "a struct tuple needs at least two elements".to_string(),
                span,
            });
        }

        // Comma-separated remaining elements (mirrors `parse_expr`'s tuple
        // loop, stepping over offside `Virtual::BlockSep` between elements).
        while self.at_tuple_continuation() {
            self.bump_into(SyntaxKind::COMMA_TOK);
            while matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            ) {
                self.bump_into(SyntaxKind::ERROR);
            }
            if self.peek_is_expr_start() {
                self.parse_range_expr();
            } else {
                // Trailing comma / missing element (`struct (1,)`): FCS reports
                // "Expected an expression after this point". Mirror `parse_expr`'s
                // tuple loop so a trailing comma is a clean error, not silently
                // accepted.
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected expression after `,` in tuple".to_string(),
                    span,
                });
                break;
            }
        }

        // The closing `)` is swallowed by the lex-filter, recovered off the
        // raw stream like a paren expression's.
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        self.builder.finish_node();
    }

    /// `true` if a `{| … |}` body is the copy-and-update form `{| e with … |}`.
    /// The cursor is at the `{|` (`self.pos`); scans the *filtered* tokens after
    /// it for a longident head followed by `with`, mirroring the copy-update
    /// arm of [`Self::classify_brace_body`].
    fn peek_anon_recd_is_copy_update(&self) -> bool {
        let mut i = self.pos + 1; // past `{|`
        if !matches!(
            self.filtered_tokens.get(i),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Ident(_) | Token::QuotedIdent(_) | Token::Global
                )),
                _
            ))
        ) {
            return false;
        }
        i += 1;
        while matches!(
            self.filtered_tokens.get(i),
            Some((Ok(FilteredToken::Raw(Token::Dot)), _))
        ) && matches!(
            self.filtered_tokens.get(i + 1),
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                _
            ))
        ) {
            i += 2;
        }
        matches!(
            self.filtered_tokens.get(i),
            Some((Ok(FilteredToken::Virtual(Virtual::With)), _))
                | Some((Ok(FilteredToken::Raw(Token::With)), _))
        )
    }

    /// Parse an anon-record body (after `{|` and trivia): an optional
    /// `<copy-source> with` prefix, then a `;`/offside-separated `RECORD_FIELD`
    /// list. Mirrors [`Self::parse_record_body`] but stops at the real filtered
    /// `|}` closer ([`Token::BarRBrace`]) instead of the swallowed `}`.
    fn parse_anon_recd_body(&mut self, copy_update: bool) {
        if copy_update {
            // Copy source — `parse_expr` stops at the `Virtual::With`.
            if self.peek_is_expr_start() {
                self.parse_expr();
            }
            if let Some((Ok(FilteredToken::Virtual(Virtual::With)), with_span)) =
                self.peek().cloned()
            {
                self.drain_raw_up_to(with_span.start);
                self.emit_text(SyntaxKind::WITH_TOK, with_span);
                self.raw_pos += 1;
                self.pos += 1;
            } else if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::With)), _))) {
                self.bump_into(SyntaxKind::WITH_TOK);
            }
        }

        // The `|}` closer is a real filtered token, so "at closer" is a plain
        // peek (no swallowed-`}` raw-stream gymnastics).
        let at_bar_rbrace = |p: &Self| {
            matches!(
                p.peek(),
                Some((Ok(FilteredToken::Raw(Token::BarRBrace)), _))
            )
        };

        if self.peek_is_record_field_start() {
            self.parse_anon_recd_field(copy_update);
        } else if !at_bar_rbrace(self) {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a field binding in anonymous record".to_string(),
                span,
            });
        }

        while !at_bar_rbrace(self) && self.consume_one_seps_group(at_bar_rbrace) {
            if at_bar_rbrace(self) || !self.peek_is_record_field_start() {
                break;
            }
            self.parse_anon_recd_field(copy_update);
        }

        // A copy-update closes with a `Virtual::End` (FCS's `appExpr OWITH …
        // OEND`) at the `|}` position; consume it zero-width so the real
        // `|}` still reaches the closer bump.
        self.eat_zero_width_virtual(Virtual::End);
    }

    /// Parse one anon-record field, reusing [`Self::parse_record_field`]. In
    /// *non*-copy-update construction FCS only accepts single-segment field
    /// names — a dotted path `{| A.B = 1 |}` is "Invalid anonymous record type"
    /// (dotted names are meaningful only for copy-update nesting,
    /// `pars.fsy:5920`). We still parse the field (lossless), but report the
    /// FCS diagnostic so a dotted construction field is a clean error, not a
    /// silently-accepted divergence. The cursor is at the field-name head ident
    /// (caller checked [`Self::peek_is_record_field_start`]).
    fn parse_anon_recd_field(&mut self, copy_update: bool) {
        if !copy_update
            && matches!(
                self.filtered_tokens.get(self.pos + 1),
                Some((Ok(FilteredToken::Raw(Token::Dot)), _))
            )
        {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "invalid anonymous record: a construction field name must be a single \
                          identifier, not a dotted path"
                    .to_string(),
                span,
            });
        }
        self.parse_record_field();
    }

    /// Lookahead disambiguating a non-`new`-headed `{ … }` brace body
    /// (`braceExprBody`). The cursor is at the `{` (`self.pos`); this scans the
    /// *filtered* tokens after it without consuming. A leading longident
    /// (`Ident (DOT Ident)*`) directly followed by `=` is a record field-list;
    /// directly followed by `with` it is a bare-longident copy-update; followed
    /// by a statement separator/closer (`{ x }`, `{ x; y }`) or a non-ident start
    /// (`inherit`/`_`) it is a computation expression. When the longident
    /// instead continues as an `appExpr` — application args or postfix
    /// (`{ f x … }`, `{ Foo.Bar () … }`) — the record-vs-CE choice can't be made
    /// by lookahead (the appExpr's `)`/`}` closers are LexFilter-swallowed), so
    /// it is deferred to [`Self::parse_app_head_brace`] via
    /// [`BraceBody::AppExprHead`]. A `new` head is handled earlier by
    /// [`Self::parse_obj_or_computation_brace`] and never reaches here.
    ///
    /// **Known limitation:** an `appExpr` copy source is recognised only when it
    /// starts with a longident head; a source starting with a non-ident atom
    /// (`{ (f x) with … }`) is still misclassified as a CE — FCS's `appExpr WITH`
    /// accepts it, but it is rare. `inherit`/`_` records are likewise deferred.
    fn classify_brace_body(&self) -> BraceBody {
        // Empty braces `{}` / `{ }` are an *empty record* (FCS's `LBRACE rbrace`
        // arm → `SynExpr.Record(None, None, [], _)`), even with a builder prefix
        // (`seq {}` is `App(seq, Record [])`). The `}` is swallowed, so detect
        // the empty brace on the raw stream right after `{`.
        if let Some((_, lbrace_span)) = self.peek()
            && matches!(
                self.next_non_trivia_raw_after(lbrace_span.end),
                Some(Token::RBrace)
            )
        {
            return BraceBody::Record { copy_update: false };
        }
        // An `inherit`-headed brace is an *inheriting record* (FCS's `recdExpr`
        // first alt, `INHERIT atomType … recdExprBindings`) → `SynExpr.Record`
        // with a `baseInfo`. Route it to the record body (which parses the
        // `inherit` clause then the fields); it is not a computation expression.
        if matches!(
            self.filtered_tokens.get(self.pos + 1),
            Some((Ok(FilteredToken::Raw(Token::Inherit)), _))
        ) {
            return BraceBody::Record { copy_update: false };
        }
        let mut i = self.pos + 1; // past `{`
        // A record starts with a longident head — an ident or F#'s `global`
        // path root (`{ global.N.F = 1 }`), matching `parse_long_ident_path`.
        // `inherit` / `_` records are deferred → CE. (A `new` head was diverted
        // earlier to the object-expression handler and never reaches here.)
        if !matches!(
            self.filtered_tokens.get(i),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Ident(_) | Token::QuotedIdent(_) | Token::Global
                )),
                _
            ))
        ) {
            // A *non-ident* head that itself begins an `appExpr` is a copy-update
            // source candidate (FCS's `recdExprCore: appExpr WITH …` admits any
            // `appExpr`, e.g. `{ [| 1 |] with … }`, `{ (x).[0] with … }`,
            // `{ !anchor with … }`). The bare-longident classifier only routed
            // ident heads to the appExpr-source path; defer these too, so the
            // leading source is parsed for real and copy-update-vs-CE is decided
            // by whether a `with` follows ([`Self::parse_app_head_brace`]).
            //
            // Three guards keep this from routing a head `parse_app_expr` can't
            // handle: (1) the head must start an *atomic* expr, so the
            // computation-expression keyword heads (`yield`/`return`/`let!`/`if`/
            // `match`/`do!`/… — not atomic) and the deferred `inherit`/`_` records
            // stay on the CE path; (2) [`Self::is_expr_start_at`] must agree the
            // head is a parseable start — notably its `(`-arm applies
            // `raw_after_lparen_starts_expr`, so a `(` opening an unsupported body
            // (`{ (let! …) }`) is *not* routed (it would otherwise reach
            // `parse_atomic_expr`'s `(` `unreachable!`); it stays a CE and errors
            // gracefully); (3) the nested-brace `{` head is excluded to avoid
            // recursing the brace disambiguation. A PREFIX_OP head (`!anchor`,
            // FCS's `PREFIX_OP atomicExpr`, an `appExpr`) is routed separately —
            // it is not in `raw_starts_atomic_expr`.
            return match self.filtered_tokens.get(i) {
                Some((Ok(FilteredToken::Raw(t)), _))
                    if raw_starts_atomic_expr(t)
                        && !matches!(t, Token::LBrace)
                        && self.is_expr_start_at(i) =>
                {
                    BraceBody::AppExprHead
                }
                Some((Ok(FilteredToken::Raw(Token::Op(text))), _)) if is_prefix_op_text(text) => {
                    BraceBody::AppExprHead
                }
                _ => BraceBody::Computation,
            };
        }
        i += 1;
        // Consume the rest of a dotted longident: (DOT Ident)*.
        while matches!(
            self.filtered_tokens.get(i),
            Some((Ok(FilteredToken::Raw(Token::Dot)), _))
        ) && matches!(
            self.filtered_tokens.get(i + 1),
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                _
            ))
        ) {
            i += 2;
        }
        // The copy-update `with` must belong to *this* brace. With the source's
        // closers (`}`/`)`) LexFilter-swallowed, a nested ident-only brace — the
        // inner `{ x }` of `{ f { x } with … }` or `{ f (seq { x }) with … }` —
        // would otherwise see the *enclosing* brace's `with` past its own swallowed
        // `}` in the filtered stream and steal the update fields. Confirm on the
        // **raw** stream that the next significant token after the head longident
        // is the `with` itself (no swallowed closer sits between), the same nesting
        // guard the object-expression handler applies. When it fails, the brace is
        // a closed ident body (`{ x }`) whose enclosing `with` is not ours: fall
        // through to the appExpr/CE route, which re-checks ownership before
        // claiming the `with`.
        let with_is_own = self
            .filtered_tokens
            .get(i - 1)
            .is_some_and(|(_, head_span)| {
                matches!(
                    self.next_non_trivia_raw_after(head_span.end),
                    Some(Token::With)
                )
            });
        match self.filtered_tokens.get(i) {
            Some((Ok(FilteredToken::Raw(Token::Equals)), _)) => {
                BraceBody::Record { copy_update: false }
            }
            // `with` surfaces as `Virtual::With` (`OWITH`) in the offside brace
            // body; accept a raw `Token::With` too for robustness.
            Some((Ok(FilteredToken::Virtual(Virtual::With)), _))
            | Some((Ok(FilteredToken::Raw(Token::With)), _))
                if with_is_own =>
            {
                BraceBody::Record { copy_update: true }
            }
            // A statement separator or block closer directly after the head
            // longident closes a single-ident computation body (`{ x }`,
            // `{ x; y }`, `{ x⏎ y }`) — there is no further `appExpr` to parse
            // and no `with` can follow, so keep the plain CE path.
            Some((Ok(FilteredToken::Raw(Token::Semi)), _))
            | Some((
                Ok(FilteredToken::Virtual(
                    Virtual::BlockSep | Virtual::End | Virtual::BlockEnd | Virtual::DeclEnd,
                )),
                _,
            ))
            | None => BraceBody::Computation,
            // The head longident continues as an `appExpr` (`{ f x … }`,
            // `{ Foo.Bar () … }`): defer the record-vs-CE decision to a real
            // parse of the leading expression (see [`Self::parse_app_head_brace`]).
            _ => BraceBody::AppExprHead,
        }
    }

    /// Parse a record-expression body (after the `{` and trivia have been
    /// consumed): for a copy-update, the `<copy-source> with` prefix (the source
    /// is a bare longident expression here) followed by the updated field list;
    /// for a field-list record, the fields directly. The shared field list and
    /// the copy-update `with`/`OEND` scaffolding live in
    /// [`Self::parse_record_fields`] / [`Self::parse_record_copy_update_tail`].
    fn parse_record_body(&mut self, copy_update: bool) {
        if copy_update {
            // Copy source — a (long)ident expression; `parse_expr` stops at the
            // `Virtual::With` (no expr production consumes it).
            if self.peek_is_expr_start() {
                self.parse_expr();
            }
            self.parse_record_copy_update_tail();
        } else if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Inherit)), _))
        ) {
            // Inheriting record `{ inherit Base(args) [; fields] }` (FCS's
            // `recdExpr`: `INHERIT atomType … recdExprBindings`). The `inherit`
            // clause becomes the record's `baseInfo`; the fields (if any) follow
            // after a separator group.
            self.parse_record_inherit();
            let at_rbrace =
                |p: &Self| matches!(p.next_non_trivia_raw_at_pos(), Some(Token::RBrace));
            while !at_rbrace(self) && self.consume_one_seps_group(at_rbrace) {
                if at_rbrace(self) || !self.peek_is_record_field_start() {
                    break;
                }
                self.parse_record_field();
            }
        } else {
            self.parse_record_fields();
        }
    }

    /// Parse the `inherit Base(args)` clause that opens an inheriting record
    /// (FCS's `recdExpr`'s `INHERIT atomType opt_HIGH_PRECEDENCE_APP
    /// opt_atomicExprAfterType`), into an [`SyntaxKind::INHERIT_MEMBER`] child of
    /// the open `RECORD_EXPR` — the same node the object-model `inherit` member
    /// (9.11a) uses, so its [`base_type`](crate::syntax::InheritMember::base_type)
    /// / [`args`](crate::syntax::InheritMember::args) facade drives the record's
    /// `baseInfo`. Unlike the member form there is no `as base` clause. The base
    /// type is `atomType`; the optional constructor args reuse the
    /// attribute-argument machinery (an adjacent `(` carries a
    /// `HighPrecedenceParenApp` marker). Caller has verified the cursor is at the
    /// raw `inherit` keyword.
    fn parse_record_inherit(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INHERIT_MEMBER));
        self.bump_into(SyntaxKind::INHERIT_TOK);
        if self.peek_starts_atomic_type() {
            self.parse_atomic_type();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a base type after `inherit`".to_string(),
                span,
            });
        }
        // Optional constructor args (`atomicExprAfterType`) — an adjacent `(` is
        // preceded by the `HighPrecedenceParenApp` marker (consumed zero-width).
        // Use the head-only [`Self::parse_atomic_expr_head`] (as `parse_new_expr`
        // does), *not* `parse_atomic_expr`: FCS's `atomicExprAfterType` consumes
        // only the argument atom, so a trailing postfix (`{ inherit B().M }`) is an
        // FCS error — folding `.M` into the base args here would wrongly accept it.
        if self.peek_is_paren_app_marker() {
            self.bump_into(SyntaxKind::ERROR);
        }
        if self.peek_starts_aftertype_arg() {
            self.parse_atomic_expr_head();
        }
        self.builder.finish_node(); // INHERIT_MEMBER
    }

    /// The copy-update tail shared by the bare-longident source
    /// ([`Self::parse_record_body`]) and the `appExpr` source
    /// ([`Self::parse_app_head_brace`]): emit the `with`, parse the updated
    /// `RECORD_FIELD` list, then consume the trailing `Virtual::End`. The copy
    /// source expression must already have been parsed and the cursor must sit
    /// at the `with` (`Virtual::With`/raw `Token::With`).
    fn parse_record_copy_update_tail(&mut self) {
        // `with`: `Virtual::With` is the `OWITH` relabel of a raw `Token::With`
        // at the same span (the `MATCH`/`WITH_TOK` pattern).
        if let Some((Ok(FilteredToken::Virtual(Virtual::With)), with_span)) = self.peek().cloned() {
            self.drain_raw_up_to(with_span.start);
            self.emit_text(SyntaxKind::WITH_TOK, with_span);
            self.raw_pos += 1;
            self.pos += 1;
        } else if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::With)), _))) {
            self.bump_into(SyntaxKind::WITH_TOK);
        }

        self.parse_record_fields();

        // Copy-update closes with a `Virtual::End` (the `OEND` of FCS's
        // `appExpr OWITH … OEND`) at the swallowed `}` position; consume it
        // zero-width so the raw `}` still reaches `bump_swallowed_closer`. A
        // *plain* record (`{ F = v }`) has no `OEND` of its own, so this lives in
        // the copy-update tail only: an ungated eat would, when the record is the
        // body of an enclosing `OWITH … OEND` construct that parks its `OEND` at
        // this same swallowed-`}` position — e.g. a get/set accessor body
        // `member P with get() = { F = v }` — steal that enclosing `OEND`,
        // collapsing the layout so a following column-0 decl is absorbed into the
        // construct (the `E_SettersMustHaveUnit01` augmentation-vs-`let` divergence).
        self.eat_zero_width_virtual(Virtual::End);
    }

    /// Parse the `;`/offside-separated `RECORD_FIELD` list — the body shared by
    /// a field-list record and a copy-update tail — starting at the first field
    /// and stopping at the swallowed `}`.
    fn parse_record_fields(&mut self) {
        // First field — required, except for an empty record (`{}`, and the
        // empty copy-update `{ x with }`), where the swallowed `}` is the next
        // significant raw token and there is simply no field to parse.
        if self.peek_is_record_field_start() {
            self.parse_record_field();
        } else if !matches!(self.next_non_trivia_raw_at_pos(), Some(Token::RBrace)) {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a field binding in record expression".to_string(),
                span,
            });
        }

        // Separator-introduced fields. FCS's `seps_block` (`pars.fsy:5767`) is a
        // *single* separator group, so a repeated separator (`{ F = 1; ; G = 2 }`)
        // is a parse error; consuming exactly one group per gap (via
        // `consume_one_seps_group`) leaves any extra to trip `parse_record_field`'s
        // recovery, matching FCS. A trailing group before `}` is tolerated.
        //
        // Unlike the anon-record *type* (whose `|}` is a real filtered token),
        // the record `}` is **swallowed** — absent from the filtered stream but
        // still next in the raw stream. So a separator that belongs to an
        // *enclosing* scope (`{ F = 1 }⏎ y`, `{ F = 1 }; y`) surfaces as a
        // filtered `Semi`/`BlockSep` right after the field, with the real `}`
        // sitting before it in the raw stream. The `at_rbrace` predicate gates on
        // the next significant *raw* token being `}`: when it is, the record is
        // closed and the following separator is the outer scope's, so stop and
        // let `bump_swallowed_closer` claim the `}`.
        let at_rbrace = |p: &Self| matches!(p.next_non_trivia_raw_at_pos(), Some(Token::RBrace));
        while !at_rbrace(self) && self.consume_one_seps_group(at_rbrace) {
            if at_rbrace(self) || !self.peek_is_record_field_start() {
                break;
            }
            self.parse_record_field();
        }
    }

    /// Parse an ident-headed brace whose head longident continues as an
    /// `appExpr` ([`BraceBody::AppExprHead`]): `{ f x with F = e }`,
    /// `{ Foo.Bar () with F = e }`, or a plain computation expression
    /// `{ f x; g y }`. FCS's `recdExprCore: appExpr WITH …` admits any `appExpr`
    /// copy source, but the appExpr's `)`/`}` closers are LexFilter-swallowed, so
    /// the leading expression is parsed *for real* under a checkpoint and the
    /// node kind ([`SyntaxKind::RECORD_EXPR`] copy-update vs.
    /// [`SyntaxKind::COMPUTATION_EXPR`]) is chosen afterwards via
    /// [`rowan::GreenNodeBuilder::start_node_at`] — mirroring the `new`-headed
    /// [`Self::parse_obj_or_computation_brace`].
    fn parse_app_head_brace(&mut self) {
        // Checkpoint before the `{` so the chosen node wraps the whole brace.
        let outer_cp = self.builder.checkpoint();
        self.bump_into(SyntaxKind::LBRACE_TOK);
        // Drain trivia between `{` and the body so it attaches to the brace node,
        // symmetric to `parse_brace_expr`.
        if let Some((_, raw_span)) = self.next_non_trivia_raw_at_pos_with_span() {
            self.drain_raw_up_to(raw_span.start);
        }
        // Checkpoint at the first body expression: the leading `appExpr` is both
        // the copy-update source candidate *and* the computation body's first
        // statement, so the CE arm gathers any further statements from here.
        let body_cp = self.builder.checkpoint();
        // Parse the source candidate at exactly `appExpr` level — FCS's
        // `recdExprCore: appExpr WITH …` only admits an `appExpr` source, so the
        // record-vs-CE decision turns on whether a `with` sits *directly* after
        // the application (before any infix / range / tuple / `:=` continuation).
        // A full `parse_expr` here would over-accept `{ a + b with … }` (an infix
        // source) as a copy-update, which FCS rejects; `parse_app_expr` stops at
        // the appExpr boundary, so its successor token is the discriminator.
        self.parse_app_expr();
        // A copy-update record iff a `with` directly follows the `appExpr` *in
        // this brace*. Discriminated on **both** streams like the `new`-headed
        // handler: the **raw** stream is the nesting guard (a genuine marker for
        // this brace is the next significant raw token; a nested brace's swallowed
        // `}` would otherwise let the filtered `peek()` surface an enclosing
        // `with`), and the filtered `peek()` confirms the `with` form
        // (`Virtual::With`/raw With).
        let with_follows = matches!(self.next_non_trivia_raw_at_pos(), Some(Token::With))
            && matches!(
                self.peek(),
                Some((
                    Ok(FilteredToken::Virtual(Virtual::With) | FilteredToken::Raw(Token::With)),
                    _
                ))
            );
        if with_follows {
            self.builder
                .start_node_at(outer_cp, FSharpLang::kind_to_raw(SyntaxKind::RECORD_EXPR));
            self.parse_record_copy_update_tail();
            self.bump_swallowed_closer(
                SyntaxKind::RBRACE_TOK,
                |t| matches!(t, Token::RBrace),
                "}",
                "record expression",
            );
        } else {
            // No `with` directly after the `appExpr`: a computation expression.
            // The `appExpr` is its first statement's head — resume the precedence
            // climb from `body_cp` so an infix/range/tuple/`:=` first statement
            // (`{ f x + g y }`) is parsed in full, exactly as `parse_expr` would.
            // A trailing `with` after a *non-appExpr* source (`{ a + b with … }`)
            // is then left for `bump_swallowed_closer` to reject — matching FCS
            // and restoring the strictness the bare-longident classifier had.
            self.continue_expr_after_app(body_cp);
            // Gather any further `;`/offside-separated statements into a
            // `SEQUENTIAL_EXPR` from `body_cp` (the shared sequence discipline,
            // `allow_typed = false` — the `computationExpr` body is a bare
            // `sequentialExpr`), matching the plain CE brace arm.
            self.finish_seq_block(body_cp, 1, false);
            self.builder.start_node_at(
                outer_cp,
                FSharpLang::kind_to_raw(SyntaxKind::COMPUTATION_EXPR),
            );
            self.bump_swallowed_closer(
                SyntaxKind::RBRACE_TOK,
                |t| matches!(t, Token::RBrace),
                "}",
                "computation expression",
            );
        }
        self.builder.finish_node();
    }

    /// One record field `F = e` → `RECORD_FIELD > [LONG_IDENT, EQUALS_TOK,
    /// <value-expr>]`. Caller must have checked [`Self::peek_is_record_field_start`].
    fn parse_record_field(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::RECORD_FIELD));
        // Field name — FCS's `RecordFieldName` (a `SynLongIdent`).
        self.parse_long_ident_path("record field");
        // `=`.
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
                message: "expected `=` after record field name".to_string(),
                span,
            });
        }
        // Value — FCS's `declExprBlock` (`recdExprCore: appExpr EQUALS
        // declExprBlock`): the full expression surface, *block-scoped*.
        //
        // An **offside** value (`{ F =⏎  1 }`) gets a `Virtual::BlockBegin`/
        // `BlockEnd` SeqBlock after `=`, with a multi-statement body sequenced by
        // `Virtual::BlockSep` (`{ F =⏎ a⏎ b }` ⇒ a `SEQUENTIAL_EXPR`) — exactly
        // like a `let`/`if`/`do!` RHS, so `parse_if_body` (which sequences the
        // block and consumes the matching `BlockEnd`) is reused.
        //
        // An **inline** value (`{ F = 1; G = 2 }`) has no block scaffolding, so
        // it is a single `parse_expr` that stops at the `;`/offside field
        // separator. It must *not* go through the seq-block gatherer, because in
        // a record `;` separates *fields*, not statements within one value.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)),
        ) {
            self.bump_into(SyntaxKind::ERROR);
            self.parse_if_body("record field", true);
        } else if self.peek_is_expr_start() {
            self.parse_expr();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a value for the record field".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// Whether the cursor is at the start of a record field — a longident head
    /// (an ident or F#'s `global` path root). The dotted tail is handled by
    /// `parse_long_ident_path`.
    fn peek_is_record_field_start(&self) -> bool {
        matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Ident(_) | Token::QuotedIdent(_) | Token::Global
                )),
                _
            ))
        )
    }

    /// Emit one FS1245 (`lexInvalidUnicodeLiteral`) per `\U........` escape in
    /// `self.source[span]` whose value exceeds U+10FFFF — the `Invalid` case of
    /// `unicodeGraphLong` (LexHelpers.fs:266). The error span lands on the
    /// offending escape (FCS reports the escape's own range), and the message
    /// echoes the eight hex digits verbatim (`\U%s`).
    ///
    /// `span` is the whole literal/fragment token: the structural delimiters
    /// (`"`, `$"`, `{`, `}`, trailing `B`) are never backslashes and never hex
    /// digits that could complete a spurious escape (the only hex-ish one, `B`,
    /// always sits past a `"`), so scanning the raw token text is equivalent to
    /// stripping the delimiters first and lets one helper serve both
    /// [`Self::parse_const_payload`] (regular/byte strings) and
    /// [`Self::parse_interp_string_expr`] (single-quoted interp fragments).
    /// The caller is responsible for only invoking this on escape-processing
    /// kinds — verbatim/triple/extended bodies have no backslash escapes.
    pub(super) fn push_long_unicode_errors(&mut self, span: Range<usize>) {
        let base = span.start;
        let text = &self.source[span];
        let bad: Vec<ParseError> = long_unicode_escapes(text)
            .into_iter()
            .filter(|e| e.value > MAX_UNICODE_SCALAR)
            .map(|e| ParseError {
                message: format!(
                    "\\U{} is not a valid Unicode character escape sequence",
                    &text[e.span.start + 2..e.span.end]
                ),
                span: base + e.span.start..base + e.span.end,
            })
            .collect();
        self.errors.extend(bad);
    }

    /// Emit FS1140 (`lexByteArrayCannotEncode`) for a byte-string literal whose
    /// decoded content has any UTF-16 code unit > 255. `span` is the whole
    /// token; `kind` selects the delimiters to strip and whether escapes are
    /// processed (regular `"…"B` yes, verbatim/triple no). One error per
    /// literal, with the FCS count in the message. A no-op for non-byte kinds.
    pub(super) fn push_byte_string_wide_error(&mut self, span: Range<usize>, kind: SyntaxKind) {
        let text = &self.source[span.clone()];
        let stripped = match kind {
            SyntaxKind::BYTE_STRING_LIT => text
                .strip_prefix('"')
                .and_then(|t| t.strip_suffix('B'))
                .and_then(|t| t.strip_suffix('"'))
                .map(|c| (c, true)),
            SyntaxKind::VERBATIM_BYTE_STRING_LIT => text
                .strip_prefix("@\"")
                .and_then(|t| t.strip_suffix('B'))
                .and_then(|t| t.strip_suffix('"'))
                .map(|c| (c, false)),
            SyntaxKind::TRIPLE_BYTE_STRING_LIT => text
                .strip_prefix("\"\"\"")
                .and_then(|t| t.strip_suffix('B'))
                .and_then(|t| t.strip_suffix("\"\"\""))
                .map(|c| (c, false)),
            // Not a byte string — nothing to check.
            _ => return,
        };
        let (content, escapes) = stripped.expect("byte-string token has its delimiters");
        let count = byte_string_wide_unit_count(content, escapes);
        if count > 0 {
            self.errors.push(ParseError {
                message: format!(
                    "This byte array literal contains {count} characters that do not encode \
                     as a single byte"
                ),
                span,
            });
        }
    }

    /// `SynExpr.Const`: a constant-literal expression. Opens `CONST_EXPR`,
    /// delegates to [`Self::parse_const_payload`] for the literal-token
    /// dispatch + validation, then closes. The pattern surface uses the
    /// same payload helper under a different outer kind (see
    /// [`Self::parse_const_pat`]).
    pub(super) fn parse_const_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::CONST_EXPR));
        self.parse_const_payload();
        self.builder.finish_node();
    }

    /// Dispatch on the current token to emit the matching literal-token
    /// kind (or, for `()`, the `LPAREN_TOK` + trivia + `RPAREN_TOK`
    /// triple) into the currently-open node. Shared between
    /// [`Self::parse_const_expr`] (wrapping `CONST_EXPR`) and
    /// [`Self::parse_const_pat`] (wrapping `CONST_PAT`) — both project
    /// to FCS's `SynConst`. The caller must have verified the leading
    /// token is a const-starter (see [`raw_starts_const_payload`] at
    /// module scope); any other token here is a parser bug.
    pub(super) fn parse_const_payload(&mut self) {
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Raw(Token::Int(text))), span)) => {
                // The Int regex over-accepts trailing underscores and any
                // width (see the `Token::Int` doc in `src/lexer/mod.rs`). FCS
                // rejects both malformed shapes and out-of-i32-range values;
                // surface the same error so we match the oracle.
                match validate_decimal_int(text) {
                    Ok(()) => {}
                    Err(DecimalIntError::Malformed) => self.errors.push(ParseError {
                        message: format!("malformed integer literal {text:?}"),
                        span: span.clone(),
                    }),
                    Err(DecimalIntError::OutOfRangeInt32) => self.errors.push(ParseError {
                        message: format!("integer literal {text:?} outside 32-bit signed range"),
                        span,
                    }),
                }
                self.bump_into(SyntaxKind::INT32_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::True | Token::False)), _)) => {
                self.bump_into(SyntaxKind::BOOL_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::XInt(text))), span)) => {
                // FCS routes bare hex/oct/bin literals through `int32`
                // (`lex.fsl`:411-419), which uses 32-bit two's complement —
                // so `0x80000000` parses to `i32::MIN`, `0xFFFFFFFF` to `-1`,
                // and only > u32 magnitudes error. The token text retains
                // the `0x`/`0o`/`0b` prefix; the normaliser is the one that
                // decodes the bit pattern, so this arm only validates range
                // before emitting INT32_LIT.
                match validate_xint_int32(text) {
                    Ok(()) => {}
                    Err(()) => self.errors.push(ParseError {
                        message: format!("integer literal {text:?} outside 32-bit signed range"),
                        span,
                    }),
                }
                self.bump_into(SyntaxKind::INT32_LIT);
            }
            Some((
                Ok(FilteredToken::Raw(Token::IntSuffixed(text) | Token::XIntSuffixed(text))),
                span,
            )) => {
                match classify_suffixed_int(text) {
                    Ok(kind) => self.bump_into(kind),
                    Err(IntSuffixedError::UnsupportedSuffix) => {
                        // The suffix table grows commit-by-commit (plan
                        // commits 5/6/7 add small/wide/native widths). Until
                        // the matching kind lands, treat unsupported suffixes
                        // as ERROR so the lossless invariant holds and the
                        // diff test for that suffix is the gating signal.
                        self.errors.push(ParseError {
                            message: format!(
                                "integer suffix in {text:?} not yet supported in phase 2"
                            ),
                            span,
                        });
                        self.bump_into(SyntaxKind::ERROR);
                    }
                    Err(IntSuffixedError::OutOfRange) => {
                        self.errors.push(ParseError {
                            message: format!("integer literal {text:?} outside its type's range"),
                            span,
                        });
                        self.bump_into(SyntaxKind::ERROR);
                    }
                }
            }
            Some((Ok(FilteredToken::Raw(Token::Float64(text))), span)) => {
                // Decimal/exponent doubles: FCS's `floatp`/`floate` regexes
                // (`lex.fsl`:283-285) require separators to sit between
                // digits, then `float(removeUnderscores ...)` parses the
                // cleaned text. Our lexer regex is more permissive, so the
                // parser checks separator placement and then defers to
                // `f64::from_str` on the cleaned text for the value-range
                // verdict.
                if !separators_well_placed(text, |c| c.is_ascii_digit())
                    || text
                        .chars()
                        .filter(|c| *c != '_')
                        .collect::<String>()
                        .parse::<f64>()
                        .is_err()
                {
                    self.errors.push(ParseError {
                        message: format!("invalid float literal {text:?}"),
                        span,
                    });
                }
                self.bump_into(SyntaxKind::IEEE64_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::Char(text))), span)) => {
                // `Char` lexer token covers both the plain char literal
                // `'a'` (→ `SynConst.Char`) and the byte-char form `'a'B`
                // (→ `SynConst.Byte`); FCS routes via two separate
                // `lex.fsl` arms (e.g. lines 519/522). We dispatch on the
                // trailing `B` byte.
                if text.ends_with('B') {
                    // FS1157: a byte-char whose value leaves the byte range —
                    // the trigraph `'\NNN'B` above 255 (error) or in 128..=255
                    // (warning, `lexInvalidTrigraphAsciiByteLiteral`), every
                    // other form above 127 (error). Range is the whole token.
                    match classify_byte_char(text) {
                        ByteCharVerdict::Error => self.errors.push(ParseError {
                            message: "This is not a valid byte character literal. \
                                      The value must be less than or equal to '\\127'B."
                                .to_string(),
                            span: span.clone(),
                        }),
                        ByteCharVerdict::Warning => self.warnings.push(ParseError {
                            message: "This is not a valid byte character literal. \
                                      The value must be less than or equal to '\\127'B.\n\
                                      Note: In a future F# version this warning will be \
                                      promoted to an error."
                                .to_string(),
                            span: span.clone(),
                        }),
                        ByteCharVerdict::Ok => {}
                    }
                    self.bump_into(SyntaxKind::BYTE_LIT);
                } else {
                    // FS1159: a `\U` escape ≥ U+10000 names either a surrogate
                    // pair or an out-of-range value, neither of which fits a
                    // single char. FCS's char arm (`lex.fsl`:572-575) folds
                    // both non-`SingleChar` cases into
                    // `lexThisUnicodeOnlyInStringLiterals`. A char literal
                    // holds at most one escape; the error range is the whole
                    // token, as FCS reports it.
                    if long_unicode_escapes(text)
                        .iter()
                        .any(|e| e.value > MAX_BMP_CODE_UNIT)
                    {
                        self.errors.push(ParseError {
                            message: "This Unicode encoding is only valid in string literals"
                                .to_string(),
                            span: span.clone(),
                        });
                    }
                    self.bump_into(SyntaxKind::CHAR_LIT);
                }
            }
            Some((Ok(FilteredToken::Raw(Token::String)), span)) => {
                // Regular `"..."` literal — `SynConst.String(_,
                // SynStringKind.Regular, _)`. `Token::String` carries no
                // text (the lexer is just a recognizer for this arm), so
                // read the source slice via the span. The byte-string
                // form `"abc"B` (lexer also produces a `Token::String`
                // because the regex consumes a trailing `B`) routes to
                // `BYTE_STRING_LIT`. The 128..=255 byte-buffer *warning*
                // (FS1253) isn't modelled (`ParseError` has no severity).
                // A single-quoted string inside an interp fill is FS3373
                // (single/verbatim enclosing) — see `check_nested_string`.
                // The byte form `"abc"B` behaves the same in FCS.
                self.check_nested_string(false, span.clone());
                let kind = if self.source[span.clone()].ends_with('B') {
                    SyntaxKind::BYTE_STRING_LIT
                } else {
                    SyntaxKind::STRING_LIT
                };
                // FS1245: a `\U` escape > U+10FFFF is `Invalid`
                // (LexHelpers.fs:266); FCS `fail`s once per offending escape
                // (`lex.fsl`:1323-1325). Both the regular and byte forms decode
                // escapes through `singleQuoteString`; verbatim/triple kinds
                // (separate lexer tokens) don't reach this arm.
                self.push_long_unicode_errors(span.clone());
                // FS1140: a byte string whose content has a code unit > 255
                // (no-op for the non-byte `STRING_LIT`).
                self.push_byte_string_wide_error(span.clone(), kind);
                self.bump_into(kind);
            }
            Some((Ok(FilteredToken::Raw(Token::VerbatimString)), span)) => {
                // Verbatim `@"..."` literal — `SynConst.String(_,
                // SynStringKind.Verbatim, _)`. Only `""` is escaped; all
                // other characters pass through. Byte-string form
                // (`@"abc"B`) routes to `VERBATIM_BYTE_STRING_LIT`.
                // Verbatim inside an interp fill is FS3373, like single.
                self.check_nested_string(false, span.clone());
                let kind = if self.source[span.clone()].ends_with('B') {
                    SyntaxKind::VERBATIM_BYTE_STRING_LIT
                } else {
                    SyntaxKind::VERBATIM_STRING_LIT
                };
                // FS1140: a literal wide char overflows the byte buffer even
                // though verbatim doesn't decode escapes.
                self.push_byte_string_wide_error(span.clone(), kind);
                self.bump_into(kind);
            }
            Some((Ok(FilteredToken::Raw(Token::TripleString)), span)) => {
                // Triple-quoted `"""..."""` literal — `SynConst.String(_,
                // SynStringKind.TripleQuote, _)`. No escapes; the content
                // between the triples is literal. Byte-string form
                // (`"""abc"""B`) routes to `TRIPLE_BYTE_STRING_LIT`.
                // FCS classifies these as `SynByteStringKind.Regular`,
                // but the parser kind preserves the source shape so the
                // normaliser can pick the right decoder.
                // A triple-quoted literal inside *any* interp fill is FS3374.
                self.check_nested_string(true, span.clone());
                let kind = if self.source[span.clone()].ends_with('B') {
                    SyntaxKind::TRIPLE_BYTE_STRING_LIT
                } else {
                    SyntaxKind::TRIPLE_STRING_LIT
                };
                // FS1140: same byte-buffer check; triple bodies are literal too.
                self.push_byte_string_wide_error(span.clone(), kind);
                self.bump_into(kind);
            }
            Some((Ok(FilteredToken::Raw(Token::InterpString(kind))), span))
                if byte_interp_lit_kind(&kind).is_some() =>
            {
                // Byte suffix on a bare interp string in const position
                // (`let $"abc"B = …`, `match … with $"abc"B`). FCS downgrades
                // the token to `BYTEARRAY` and parses it through the shared
                // `constant` production as `SynPat.Const(SynConst.Bytes(_,
                // Regular, _))` + FS3377 — the pattern-side mirror of the
                // expression recovery in `parse_interp_string_expr`. The
                // `raw_starts_const_payload` gate only admits the bare byte
                // forms, so `byte_interp_lit_kind` is always `Some` here.
                // A byte-interp opener used as a pattern inside a fill (e.g.
                // `$"x={ fun $"y"B -> 1 }"`) is *both* FS3377 (byte) and the
                // FS3373/FS3374 nesting error, so consult the nesting check
                // too — the expression form handles this in
                // `parse_interp_string_expr`.
                self.check_interp_nesting(&kind, span.clone());
                // FS1245: a single-quoted byte interp (`$"…"B`) still decodes
                // `\U` escapes through `singleQuoteString`, so FCS reports an
                // out-of-range escape here too (before the FS3377). The
                // expression form is scanned in `parse_interp_string_expr`;
                // this is the const/pattern mirror. Verbatim/triple byte interp
                // (`$@"…"B` / `$"""…"""B`) don't honour backslash escapes.
                if matches!(kind, crate::lexer::InterpKind::BeginEnd { .. }) {
                    self.push_long_unicode_errors(span.clone());
                }
                let lit_kind = byte_interp_lit_kind(&kind).expect("gated to bare byte interp");
                self.recover_byte_interp(lit_kind, span);
            }
            Some((Ok(FilteredToken::Raw(Token::BigNum(text))), span)) => {
                // `SynConst.UserNum(value, suffix)` — lex.fsl:511-513 splits
                // the token at the last character, with `_` removed from
                // `value`. The FCS regex (`integer` at lex.fsl:249) requires
                // separators to sit between digits; check that, since our
                // lexer is more permissive.
                let body = &text[..text.len() - 1];
                if !separators_well_placed(body, |c| c.is_ascii_digit()) {
                    self.errors.push(ParseError {
                        message: format!("invalid bignum literal {text:?}"),
                        span,
                    });
                }
                self.bump_into(SyntaxKind::USER_NUM_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::Decimal(text))), span)) => {
                // `SynConst.Decimal` — lex.fsl:489-497 strips the trailing
                // `m`/`M`, removes `_`, then `System.Decimal.Parse(...)` with
                // `AllowExponent | Number` under `InvariantCulture`. FCS's
                // float regex requires `_` between digits only; check that
                // here (our lexer regex over-accepts `1_.5m` and similar).
                // Out-of-range values (FS1154 `lexOutsideDecimal`) belong
                // to the diagnostic phase.
                let body = &text[..text.len() - 1];
                if !separators_well_placed(body, |c| c.is_ascii_digit()) {
                    self.errors.push(ParseError {
                        message: format!("invalid decimal literal {text:?}"),
                        span,
                    });
                }
                self.bump_into(SyntaxKind::DECIMAL_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::Float32(text))), span)) => {
                // Decimal/dotless single-precision floats: FCS's `evalFloat`
                // (lex.fsl:212) strips the trailing `f`/`F` then removes
                // underscores before `float32(...)`. Match the same prep
                // before validating with Rust's `f32::from_str`.
                if !float32_body_parses(text) {
                    self.errors.push(ParseError {
                        message: format!("invalid float32 literal {text:?}"),
                        span,
                    });
                }
                self.bump_into(SyntaxKind::IEEE32_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::XIEEE32(text))), span)) => {
                // Hex-bit-pattern singles: FCS strips `lf`, removes
                // underscores, parses as int64, range-checks `0..=0xFFFFFFFF`,
                // and bit-casts via `BitConverter.ToSingle`
                // (`lex.fsl`:498-504). The 32-bit-fit check is the only
                // place this differs from XIEEE64.
                match validate_xieee32(text) {
                    Ok(()) => {}
                    Err(()) => self.errors.push(ParseError {
                        message: format!("hex float32 literal {text:?} body doesn't fit 32 bits"),
                        span,
                    }),
                }
                self.bump_into(SyntaxKind::IEEE32_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::XIEEE64(text))), span)) => {
                // Hex-bit-pattern doubles: FCS strips the `LF` suffix,
                // removes underscores, parses as `int64`, and bit-casts via
                // `BitConverter.Int64BitsToDouble` (`lex.fsl`:506-509). Any
                // 64-bit value is a valid bit pattern; only > 64-bit
                // magnitudes error. We range-check here and let the
                // normaliser do the bit-cast.
                match validate_xieee64(text) {
                    Ok(()) => {}
                    Err(()) => self.errors.push(ParseError {
                        message: format!("hex float literal {text:?} body doesn't fit 64 bits"),
                        span,
                    }),
                }
                self.bump_into(SyntaxKind::IEEE64_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::KeywordString(_))), _)) => {
                // `__SOURCE_DIRECTORY__` / `__SOURCE_FILE__` / `__LINE__`
                // — `SynConst.SourceIdentifier(spelling, expanded, range)`
                // in FCS (`pars.fsy:3475-3477`). The expanded value
                // (current source dir / file path / 1-based line) is
                // computed by the consumer, since byte spans alone don't
                // carry the file path or 1-based line; we just stamp the
                // spelling token under `SOURCE_IDENTIFIER_LIT`.
                self.bump_into(SyntaxKind::SOURCE_IDENTIFIER_LIT);
            }
            Some((Ok(FilteredToken::Raw(Token::LParen)), _)) => {
                // `SynConst.Unit` = `( <only-trivia> )`. `peek_is_expr_start`
                // verified the next non-trivia *raw* token is `RParen` (the
                // lexfilter mirrors FCS's outer-wrapper expansion to
                // `RPAREN_*_COMING_SOON`/`RPAREN_IS_HERE`, all of which map
                // to `FSharpTokenKind.None` — so `RParen` never reaches the
                // filtered stream and we read it from the raw stream).
                // Trivia between the parens lives in the raw stream and
                // lands under `CONST_EXPR` via the drain inside
                // `bump_swallowed_rparen`. Paren *expressions* (anything
                // non-trivia between) are Phase 3.
                self.bump_into(SyntaxKind::LPAREN_TOK);
                self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
            }
            // The caller (`parse_const_expr` or `parse_const_pat`) is only
            // entered after a const-starter check, so any other token here
            // is a parser bug, not bad input.
            other => {
                unreachable!("parse_const_payload called with non-const-starter token: {other:?}",)
            }
        }
    }

    /// Parse the identifier-headed expression form. With a single segment
    /// (`x`, `` ``foo bar`` ``), produces FCS's optimised `SynExpr.Ident`:
    /// `IDENT_EXPR > IDENT_TOK`. With two or more dot-separated segments
    /// (`Foo.Bar.Baz`), produces `SynExpr.LongIdent`:
    /// `LONG_IDENT_EXPR > LONG_IDENT > [IDENT_TOK, DOT_TOK, IDENT_TOK, …]`.
    /// A trailing dot (`Foo.\n`) is a parse error in Phase 2 — FCS supports
    /// trailing-dot recovery for IntelliSense but we punt on that here.
    ///
    /// A `.[` indexer (`arr.[0]`, phase 10.16a) is *not* part of the path: a
    /// dot followed by `[` ends the long-ident here (the head stays a single
    /// `SynExpr.Ident` / the shorter `LongIdent`), leaving the `.[…]` for the
    /// postfix tail ([`Self::parse_postfix_dot_tail`]) to fold into a
    /// `DotIndexedGet`. This matches FCS, where `arr.[0]` is
    /// `DotIndexedGet(Ident "arr", …)`, not a one-segment long-ident.
    pub(super) fn parse_ident_expr(&mut self) {
        // Emit the head ident, then decide single-vs-long *after* it. Deciding
        // post-bump keeps the lookahead cursor-relative
        // ([`Self::at_raw_adjacent_dot`]): scanning the raw stream from the
        // start for every name would be O(n²) over a file. The `checkpoint`
        // defers the wrapper-node choice (`IDENT_EXPR` vs `LONG_IDENT_EXPR`)
        // until the head token is in the tree. The caller's dispatch guarantees
        // the current token is an `Ident`/`QuotedIdent`.
        let cp = self.builder.checkpoint();
        self.bump_into(SyntaxKind::IDENT_TOK);

        // A long-ident path continues iff a *raw-adjacent* `.` follows that is
        // not a `.[` indexer. The raw-adjacency rejects a `.` separated from
        // the head by a LexFilter-swallowed `)` — in `(f y).Bar` the `)` is
        // gone from the filtered stream, so `y` would otherwise look long;
        // keeping it a single `SynExpr.Ident` lets `.Bar` attach to the paren.
        // A `.[` indexer is left for the postfix tail
        // ([`Self::parse_postfix_dot_tail`]). A `.( :: ).<int>` cons-field
        // qualification is likewise *not* a path extension (it folds the head
        // into a `LIBRARY_ONLY_FIELD_GET_EXPR` in the postfix tail), so the head
        // stays a single `SynExpr.Ident` — `cons.( :: ).1` is
        // `LibraryOnlyUnionCaseFieldGet(Ident "cons", …)`, not over a one-segment
        // `LongIdent`.
        if !self.at_long_ident_segment() || self.at_cons_field_get(self.pos) {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::IDENT_EXPR));
            self.builder.finish_node();
            return;
        }

        self.build_long_ident_expr(cp);
    }

    /// Build a [`SyntaxKind::LONG_IDENT_EXPR`] from a head segment already bumped
    /// at `cp`, folding the `(DOT IDENT)+` / operator-value continuation. The
    /// caller must have confirmed [`Self::at_long_ident_segment`] (a raw-adjacent
    /// `.member`) follows the head. Shared by [`Self::parse_ident_expr`] (ident
    /// head) and [`Self::parse_base_expr`] (the `base` keyword head, emitted as
    /// an `IDENT_TOK` matching FCS's `SynExpr.Ident("base")`).
    fn build_long_ident_expr(&mut self, cp: rowan::Checkpoint) {
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_EXPR));
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        // Each iteration consumes `DOT IDENT`, or a trailing operator-value
        // segment `DOT ( op )` (FCS's `mkSynDot` folding an `opName`
        // qualification onto the long-ident — `Checked.(-)` →
        // `LongIdent(["Checked"; "op_Subtraction"])`). The loop stops at a
        // non-adjacent dot (swallowed closer ahead, e.g. `(a.b).c`) or a `.[`
        // indexer — both belong outside the path. A dot followed by neither an
        // ident nor an operator-value is a phase-2 parse error (no trailing-dot
        // recovery).
        // A library-only cons-cell field qualification `.( :: ).<int>` is *not* a
        // long-ident segment — it folds the ident head into a
        // `LIBRARY_ONLY_FIELD_GET_EXPR` instead. Stop the path here and leave it
        // for the postfix tail ([`Self::parse_postfix_tail`]), which the atomic
        // wrapper runs after this head.
        while self.at_long_ident_segment() && !self.at_cons_field_get(self.pos) {
            let dot_span = match self.peek().cloned() {
                Some((_, span)) => span,
                None => unreachable!("at_raw_adjacent_dot implies a peeked Dot"),
            };
            self.bump_into(SyntaxKind::DOT_TOK);
            match self.peek().cloned() {
                Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), _)) => {
                    self.bump_into(SyntaxKind::IDENT_TOK);
                }
                // `.(*)` — glued multiply operator-value qualification.
                Some((Ok(FilteredToken::Raw(Token::LParenStarRParen)), _)) => {
                    self.consume_star_op_value();
                }
                // `.( op )` / `.( * )` — operator-value qualification (the
                // spaced star is the multiply operator here).
                Some((Ok(FilteredToken::Raw(Token::LParen)), _))
                    if self.at_dot_paren_op_value(self.pos) =>
                {
                    self.consume_paren_op_value();
                }
                // `.(|Foo|_|)` — active-pattern-name qualification (`opName`
                // folded onto the path, FCS's `mkSynDot`): `Foo.(|Bar|_|)` →
                // `LongIdent(["Foo"; "|Bar|_|"])`. The dot was bumped above, so
                // the cursor sits at the segment's `(`.
                Some((Ok(FilteredToken::Raw(Token::LParen)), _)) if self.at_active_pat_name() => {
                    self.parse_active_pat_name();
                }
                _ => {
                    // Report the error at the dot's span and stop extending the
                    // path; the next decl iteration handles whatever followed.
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

    /// `base.Member` — base-class member access (FCS's `BASE DOT
    /// atomicExprQualification`, `pars.fsy:5276`). FCS represents the `base`
    /// keyword as `SynExpr.Ident("base")` and folds the `.Member` qualification
    /// onto it via `mkSynDot`, so `base.Foo` is `SynExpr.LongIdent(["base";
    /// "Foo"])` — exactly the shape an ordinary ident head produces. We mirror
    /// that: emit `base` as an `IDENT_TOK` head, then reuse the shared long-ident
    /// continuation ([`Self::build_long_ident_expr`]). The `.` qualification is
    /// **mandatory** — a bare `base` is an FCS parse error ("Expected '.'"); we
    /// emit the same diagnostic and leave the `base` as a lone `IDENT_EXPR` for
    /// losslessness. The caller's dispatch has verified the cursor is at `base`.
    pub(super) fn parse_base_expr(&mut self) {
        let cp = self.builder.checkpoint();
        self.bump_into(SyntaxKind::IDENT_TOK);
        if self.at_long_ident_segment() && !self.at_cons_field_get(self.pos) {
            // `base.Member(.Member)*` — fold the `.member` chain into a
            // `LONG_IDENT_EXPR` (FCS's `LongIdent(["base"; …])`).
            self.build_long_ident_expr(cp);
        } else if self.at_raw_adjacent_dot() {
            // `base.[i]` (dotted indexer) or `base.( :: ).<int>` (cons-field) — and
            // the trailing-dot recovery `base.`. FCS's `atomicExprQualification`
            // admits these, keeping `base` a bare `SynExpr.Ident`, so leave it a
            // bare `IDENT_EXPR` head; the postfix tail ([`Self::parse_postfix_tail`])
            // then builds the `DotIndexedGet` / `LibraryOnlyUnionCaseFieldGet`
            // (`base.[i]` = `DotIndexedGet(Ident "base", i)`), or reports the
            // trailing-dot error for a dangling `base.`.
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::IDENT_EXPR));
            self.builder.finish_node();
        } else {
            // A bare `base` with no `.` qualification — an FCS parse error
            // ("Expected '.'"); emit the same diagnostic and keep `base` a
            // lossless `IDENT_EXPR`.
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::IDENT_EXPR));
            self.builder.finish_node();
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `.` and a member after `base`".to_string(),
                span,
            });
        }
    }

    /// `global` / `global.Path` — the global-namespace root (FCS's `GLOBAL DOT
    /// …` path head). FCS represents the `global` keyword as an *identifier*
    /// whose `idText` is the single-backtick-quoted `` `global` `` (the keyword
    /// reused as an identifier), then folds any `.Member` qualification onto it
    /// via `mkSynDot`, so `global.System.Console` is `SynExpr.LongIdent(["`\
    /// global`"; "System"; "Console"])` — exactly the shape an ordinary ident
    /// head produces. We mirror that: emit `global` as an `IDENT_TOK` head, then
    /// reuse the shared long-ident continuation ([`Self::build_long_ident_expr`]).
    ///
    /// The **critical difference from [`Self::parse_base_expr`]**: a `.`
    /// qualification is *optional*. A bare `global` is **valid** — FCS builds a
    /// *single*-segment `SynExpr.LongIdent(["`global`"])` for it (NOT a
    /// `SynExpr.Ident`), so we emit a one-segment `LONG_IDENT_EXPR` here, not an
    /// `IDENT_EXPR`. A `base` with no `.` is an error; a `global` with no `.` is
    /// not. The caller's dispatch has verified the cursor is at `global`.
    pub(super) fn parse_global_expr(&mut self) {
        let cp = self.builder.checkpoint();
        self.bump_into(SyntaxKind::IDENT_TOK);
        if self.at_long_ident_segment() && !self.at_cons_field_get(self.pos) {
            // `global.Member(.Member)*` — fold the `.member` chain into a
            // `LONG_IDENT_EXPR` (FCS's `LongIdent(["`global`"; …])`).
            self.build_long_ident_expr(cp);
        } else {
            // A bare `global` (or a `global.[i]` indexer / `global.( :: )`
            // cons-field / trailing-dot `global.`, all left for the postfix
            // tail) — FCS keeps `global` a single-segment `SynExpr.LongIdent`
            // whether or not a `.`-qualification follows, so wrap it as a
            // one-segment `LONG_IDENT_EXPR` (its `LONG_IDENT` holds just the
            // `global` `IDENT_TOK`). Unlike `base`, no error is reported.
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_EXPR));
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
            self.builder.finish_node();
            self.builder.finish_node();
        }
    }

    /// `true` if the current filtered token is a `.` that is *also* the next
    /// raw significant token (same start offset) — a dot not separated from the
    /// cursor by a LexFilter-swallowed closer. Cursor-relative (amortised O(1)
    /// via [`Self::next_non_trivia_raw_at_pos_with_span`]), so it is safe on the
    /// per-identifier hot path. The swallowed `)` in `(f y).Bar` surfaces here
    /// as a raw token with an earlier start, so this returns `false` and the
    /// `.Bar` is left for the enclosing construct. Shared by the long-ident
    /// loop and the postfix tail.
    fn at_raw_adjacent_dot(&self) -> bool {
        let Some((Ok(FilteredToken::Raw(Token::Dot)), dot_span)) = self.peek() else {
            return false;
        };
        self.next_non_trivia_raw_at_pos_with_span()
            .map(|(_, s)| s.start)
            == Some(dot_span.start)
    }

    /// `true` if the current filtered token is *also* the next significant raw
    /// token (same start offset) — i.e. the cursor has not advanced past a
    /// LexFilter-swallowed closer (`)`/`}`/`]`). The generalisation of
    /// [`Self::at_raw_adjacent_dot`] to any cursor token: the swallowed closer
    /// surfaces in the raw stream with an earlier start, so the equality fails.
    /// Used by the dot-lambda body guard so `(_.) x` recovers at `_.` (leaving
    /// the `)` and the outside `x` to the enclosing paren) rather than dragging
    /// `x` in as the body. A cursor at EOF (no filtered token, no raw token) is
    /// vacuously adjacent.
    fn cursor_is_raw_adjacent(&self) -> bool {
        match (self.peek(), self.next_non_trivia_raw_at_pos_with_span()) {
            (Some((_, filtered_span)), Some((_, raw_span))) => {
                filtered_span.start == raw_span.start
            }
            (None, None) => true,
            _ => false,
        }
    }

    /// `true` if the filtered token *after* the current one is `[` — i.e. a
    /// `.[` dotted indexer rather than a `.member` access. Callers check the
    /// current token is a `.` first (typically via [`Self::at_raw_adjacent_dot`]).
    fn dot_is_followed_by_lbrack(&self) -> bool {
        matches!(
            self.filtered_tokens.get(self.pos + 1),
            Some((Ok(FilteredToken::Raw(Token::LBrack)), _)),
        )
    }

    /// `true` if the cursor is at a raw-adjacent `.` that continues a
    /// *long-ident path* — i.e. a `.member`, not a `.[` indexer (which the
    /// postfix tail handles separately) and not a dot reached across a
    /// swallowed closer. The shared gate for the single-vs-long decision and
    /// the long-ident / dot-get segment loops.
    fn at_long_ident_segment(&self) -> bool {
        self.at_raw_adjacent_dot() && !self.dot_is_followed_by_lbrack()
    }

    /// `true` when the filtered token at `idx` opens FSharp.Core's library-only
    /// cons-cell field qualification `.( :: ).<int>` — FCS's `LPAREN COLON_COLON
    /// rparen DOT INT32` (`pars.fsy:5351`, `SynExpr.LibraryOnlyUnionCaseFieldGet`).
    /// The closing `)` is LexFilter-swallowed, so the filtered run is exactly
    /// `Dot LParen ColonColon Dot Int`. Used both to stop the ident-head long-ident
    /// loop ([`Self::build_long_ident_expr`]) before this qualification and to
    /// dispatch it in [`Self::parse_postfix_tail`].
    fn at_cons_field_get(&self, idx: usize) -> bool {
        let raw = |i: usize| match self.filtered_tokens.get(i) {
            Some((Ok(FilteredToken::Raw(t)), _)) => Some(t),
            _ => None,
        };
        // FCS's grammar token here is `INT32` — an int32-typed literal: any
        // unsuffixed decimal (`Token::Int`) / radix (`Token::XInt`,
        // `0x`/`0o`/`0b`), or one carrying the int32 `l` suffix (`1l`, `0x1l`,
        // lexed `IntSuffixed`/`XIntSuffixed` and classified `INT32_LIT`). A
        // non-int32 suffix (`1L` int64, `1u` uint32, …) is a different int type,
        // which FCS rejects here (FS0010).
        let is_int32 = |t: Option<&Token>| match t {
            Some(Token::Int(_) | Token::XInt(_)) => true,
            Some(Token::IntSuffixed(s) | Token::XIntSuffixed(s)) => {
                matches!(classify_suffixed_int(s), Ok(SyntaxKind::INT32_LIT))
            }
            _ => false,
        };
        matches!(raw(idx), Some(Token::Dot))
            && matches!(raw(idx + 1), Some(Token::LParen))
            && matches!(raw(idx + 2), Some(Token::ColonColon))
            && matches!(raw(idx + 3), Some(Token::Dot))
            && is_int32(raw(idx + 4))
    }

    /// Parse FSharp.Core's library-only cons-cell field read `.( :: ).<int>` as a
    /// [`SyntaxKind::LIBRARY_ONLY_FIELD_GET_EXPR`] wrapping the head at `cp`
    /// (`SynExpr.LibraryOnlyUnionCaseFieldGet`, `pars.fsy:5351`). The caller has
    /// verified [`Self::at_cons_field_get`]. Shape `[<object>, DOT_TOK, LPAREN_TOK,
    /// COLON_COLON_TOK, RPAREN_TOK, DOT_TOK, INT32_LIT]`; the closing `)` is
    /// LexFilter-swallowed and recovered. The union-case name is always the cons
    /// operator (`op_ColonColon`), so only the field number is carried. The set
    /// form `… <- rhs` is the enclosing [`SyntaxKind::ASSIGN_EXPR`] over this get.
    fn parse_cons_field_get_tail(&mut self, cp: rowan::Checkpoint, head_start: Option<usize>) {
        self.builder.start_node_at(
            cp,
            FSharpLang::kind_to_raw(SyntaxKind::LIBRARY_ONLY_FIELD_GET_EXPR),
        );
        self.bump_into(SyntaxKind::DOT_TOK);
        self.bump_into(SyntaxKind::LPAREN_TOK);
        self.bump_into(SyntaxKind::COLON_COLON_TOK);
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        self.bump_into(SyntaxKind::DOT_TOK);
        self.bump_into(SyntaxKind::INT32_LIT);
        self.builder.finish_node();
        // FCS flags the whole `obj.( :: ).<int>` expression as library-only
        // (FS0042, "only for use in the F# library") — a parse error it still
        // builds the node for. Mirror that diagnostic over the same span (the
        // object's start through the field number, now `raw_consumed_end`) so the
        // construct parses faithfully; the tree is the LSP-usable value.
        if let Some(start) = head_start {
            self.errors.push(ParseError {
                message: "this construct is for use only in the F# library".to_string(),
                span: start..self.raw_consumed_end,
            });
        }
    }

    /// The postfix tail of FCS's left-recursive `atomicExpr`, applied after
    /// every atomic head ([`Self::parse_atomic_expr`]). One interleaved loop
    /// over the three postfix continuations, each wrapping the already-emitted
    /// head via `cp`, left-associatively:
    ///
    /// * `.[index]` → [`SyntaxKind::DOT_INDEXED_GET_EXPR`] (FCS `DotIndexedGet`);
    /// * `.member(.member)*` → [`SyntaxKind::DOT_GET_EXPR`] (FCS `DotGet`);
    /// * high-precedence (adjacent) paren application `f(x)` →
    ///   [`SyntaxKind::APP_EXPR`] with the
    ///   [`SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK`] marker
    ///   (FCS `App(ExprAtomicFlag.Atomic, …)`);
    /// * high-precedence (adjacent) bracket indexer `arr[i]` →
    ///   [`SyntaxKind::APP_EXPR`] with the
    ///   [`SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK`] marker, whose argument is
    ///   the bracketed list literal (FCS `App(Atomic, head,
    ///   ArrayOrListComputed[…])`) — the *non-dotted* indexer, distinct from the
    ///   dotted `.[index]` `DotIndexedGet` above.
    ///
    /// Interleaving the dot/index access with the high-precedence application
    /// at this single (atomic) level is phase **10.16b**: it is what makes
    /// `f(x).Bar` parse as `DotGet(App(f, (x)), [Bar])` and `obj.M(x).N` as
    /// `DotGet(App(obj.M, (x)), [N])` — the dot binds to the *whole* preceding
    /// application. Because both the head and every argument of
    /// [`Parser::parse_app_expr`] (whitespace application) go through here, the
    /// adjacent application also binds tighter than the whitespace one:
    /// `f g(x)` is `App(f, App(g, (x)))`, not `(f g) (x)`.
    ///
    /// A pure identifier chain `a.b.c` never reaches the `DotGet` arm —
    /// [`Self::parse_ident_expr`] has already consumed it as a
    /// `LONG_IDENT_EXPR` — so that arm only fires after a non-ident head
    /// (paren, indexer, a high-precedence application, …), matching FCS's
    /// `mkSynDot` (which keeps an ident-rooted chain as `SynExpr.LongIdent`).
    fn parse_postfix_tail(&mut self, cp: rowan::Checkpoint, head_start: Option<usize>) {
        loop {
            // Only a raw-adjacent `.` continues a postfix access — a dot reached
            // only by crossing a LexFilter-swallowed `)` belongs to an
            // enclosing construct (see [`Self::at_raw_adjacent_dot`]).
            if self.at_raw_adjacent_dot() {
                if self.at_cons_field_get(self.pos) {
                    // `.( :: ).<int>` — FSharp.Core's library-only cons-cell field
                    // read (`SynExpr.LibraryOnlyUnionCaseFieldGet`). Checked first:
                    // its `.( :: )` would otherwise be misread by the `.( op )`
                    // operator-value arm and its trailing `.<int>` would dangle.
                    self.parse_cons_field_get_tail(cp, head_start);
                } else if self.dot_is_followed_by_lbrack() {
                    // `.[` — a dotted indexer read.
                    self.parse_dot_indexed_get_tail(cp);
                } else if matches!(
                    self.filtered_tokens.get(self.pos + 1),
                    Some((
                        Ok(FilteredToken::Raw(
                            Token::Ident(_) | Token::QuotedIdent(_) | Token::LParenStarRParen
                        )),
                        _
                    )),
                ) || self.at_dot_paren_op_value(self.pos + 1)
                    || self.at_active_pat_name_at(self.pos + 1)
                {
                    // `.member` — postfix member access (one or more segments) —
                    // or a `.( op )` / `.(*)` / `.( * )` operator-value or
                    // `.(|Foo|_|)` active-pattern-name qualification off a
                    // non-ident head (`(id 1).(+)` → `DotGet(Paren …, ["+"])`;
                    // `(id 1).(|Bar|_|)` → `DotGet(Paren …, ["|Bar|_|"])`). FCS's
                    // `atomicExprQualification` accepts `identOrOp`, and `opName`
                    // includes the active-pattern names.
                    self.parse_dot_get_tail(cp);
                } else {
                    // A dangling `.` after a non-ident head (e.g. `(f x).`):
                    // mirror `parse_ident_expr`'s trailing-dot error and stop,
                    // consuming the dot so the round-trip stays lossless.
                    // (Identifier heads never reach here — their trailing dot is
                    // handled in `parse_ident_expr`.)
                    let dot_span = self
                        .peek()
                        .map(|(_, span)| span.clone())
                        .expect("at_raw_adjacent_dot implies a peeked Dot");
                    self.errors.push(ParseError {
                        message: "trailing dot in long identifier path".to_string(),
                        span: dot_span,
                    });
                    self.bump_into(SyntaxKind::DOT_TOK);
                    return;
                }
            } else if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::QMark)), _)))
                && self.cursor_is_raw_adjacent()
            {
                // `a?b` — the dynamic-lookup operator (FCS's `atomicExpr QMARK
                // dynamicArg`). `?` is a postfix operator at `.`'s precedence, so
                // it lives in this tail loop: a following `.member` / adjacent
                // application then chains onto the *whole* `Dynamic` (`a?b.c` =
                // `DotGet(Dynamic(a, b), [c])`), and a `?`-chain nests left
                // (`a?b?c` = `Dynamic(Dynamic(a, b), c)`). Unlike `.`, FCS imposes
                // no *whitespace* adjacency requirement (`a ? b` parses), but the
                // `?` must still be the next *raw* token — a `?` reached only by
                // crossing a LexFilter-swallowed `)` belongs to the enclosing
                // construct (`(a)?b` is `Dynamic(Paren a, b)`, not `Paren(a?b)`),
                // exactly as [`Self::at_raw_adjacent_dot`] guards `.`. The operator
                // is otherwise unambiguous after an atomic head (the
                // optional-argument `?ident` form only arises with *no* preceding
                // atomic expr).
                self.parse_dynamic_tail(cp);
            } else if self.peek_high_precedence_paren_app() {
                // `f(x)` — adjacent (no-whitespace) paren application. LexFilter
                // emits the marker only when `(` is adjacent to an *ident* /
                // member name, so it fires for `f(`, `.Bar(`, … but **not** for a
                // `(` after a `)`. Wrap the accumulated head + the `(…)` argument
                // in an atomic `APP_EXPR`. The argument is parsed *head-only*
                // ([`Self::parse_atomic_expr_head`]): a following `.Bar` (or a
                // member-adjacent `(y)`, as in `f(x).Bar(y)`) chains onto the
                // *whole* application via this loop, not onto the argument
                // (`f(x).Bar` = `DotGet(App(f,(x)), …)`). A *paren*-adjacent next
                // call (`f(x)(y)`) is markerless and so is left to
                // [`Parser::parse_app_expr`]'s whitespace loop — matching FCS,
                // which makes that second application `NonAtomic`.
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_EXPR));
                self.bump_into(SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK);
                self.parse_atomic_expr_head();
                self.check_adjacent_malformed_numeric();
                self.builder.finish_node();
            } else if self.peek_high_precedence_brack_app() {
                // `arr[i]` — adjacent (no-whitespace) bracket indexer. LexFilter
                // emits the marker only when `[` is adjacent to an *ident*, so it
                // fires for `arr[`, `.Bar[`, … but **not** for a `[` after `)` /
                // `]` (those stay markerless NonAtomic whitespace applications,
                // handled by [`Parser::parse_app_expr`]). FCS lowers this to an
                // *atomic application* of the accumulated head to the bracketed
                // list literal (`App(Atomic, head, ArrayOrListComputed[…])`,
                // `pars.fsy:5242`) — **not** a `DotIndexedGet` (that is the dotted
                // `arr.[i]`). Wrap head + the `[…]` argument in an atomic
                // `APP_EXPR`; the `[…]` is the same list-literal expression as a
                // bare `[i]` ([`Self::parse_array_or_list_expr`]), so a tuple /
                // range / nested index inside the brackets falls out for free. A
                // trailing `.Bar` or a second markerless `[j]` chains onto the
                // *whole* application via this loop / the whitespace-app loop.
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_EXPR));
                self.bump_into(SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK);
                self.parse_array_or_list_expr();
                self.builder.finish_node();
            } else if self.peek_is_tyapp_marker() && self.prev_filtered_is_measure_numeric() {
                // `1.0<ml>` — a measure-annotated numeric constant. FCS's
                // `rawConstant HIGH_PRECEDENCE_TYAPP measureTypeArg`
                // (`pars.fsy:3521`) shares the adjacency marker with the
                // type-application form below, but the head here is a numeric
                // literal, so it folds into a single `SynConst.Measure` rather
                // than a `SynExpr.TypeApp`. Wrap the accumulated `CONST_EXPR` at
                // `cp` in a `MEASURE_LIT_EXPR`.
                self.parse_measure_lit_tail(cp);
            } else if self.peek_is_tyapp_marker() {
                // `f<int>` — adjacent generic type application. LexFilter emits
                // the `HighPrecedenceTyApp` virtual between the head and an
                // adjacent type-application `<` (and rewrites that `<` to
                // `Less(true)`, the matching `>` to `Greater(true)`). Wrap the
                // accumulated head + the `<…>` block in a `TYPE_APP_EXPR`. The
                // result is itself an `atomicExpr`, so a trailing `.Bar` or a
                // `>`-adjacent `(…)` (marked `HighPrecedenceParenApp` by the
                // LexFilter, as in `ResizeArray<_>()`) chains onto the *whole*
                // type application via the next iteration of this loop.
                self.parse_type_app_tail(cp);
            } else {
                return;
            }
        }
    }

    /// `true` when the next filtered token is a
    /// [`Virtual::HighPrecedenceTyApp`] — the adjacency marker LexFilter
    /// inserts between an ident / member name and an adjacent
    /// type-application `<` (`f<int>`, distinct from a comparison `f < int`).
    fn peek_is_tyapp_marker(&self) -> bool {
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)), _)),
        )
    }

    /// `true` when the filtered token immediately before the cursor is a
    /// numeric literal that can carry a unit-of-measure annotation
    /// (`1.0<ml>`, `5<m>`). The LexFilter emits the *same*
    /// [`Virtual::HighPrecedenceTyApp`] marker for two FCS productions that
    /// share the adjacency trigger (`is_typar_application_trigger` fires on
    /// numeric literals as well as idents): the expression-level type
    /// application `atomicExpr HIGH_PRECEDENCE_TYAPP typeArgsActual`
    /// (`pars.fsy:5252`) **and** the measured constant `rawConstant
    /// HIGH_PRECEDENCE_TYAPP measureTypeArg` (`pars.fsy:3521`). The two are
    /// disambiguated by the head: a measured numeric constant is a `SynConst`
    /// carrying a `SynMeasure`, *not* a `SynExpr.TypeApp`, so
    /// [`Self::parse_postfix_tail`]'s type-application arm must decline it in
    /// favour of [`Self::parse_measure_lit_tail`]. Mirrors the type-side
    /// `head_is_app_con` gate that keeps `42<int>` out of
    /// [`SyntaxKind::APP_TYPE`] (`parse_atomic_type`).
    ///
    /// The filtered stream is trivia-free and the measure marker directly
    /// follows the numeric token, so the token at `self.pos - 1` is exactly the
    /// head literal.
    fn prev_filtered_is_measure_numeric(&self) -> bool {
        let Some(prev) = self.pos.checked_sub(1) else {
            return false;
        };
        matches!(
            self.filtered_tokens.get(prev),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Int(_)
                        | Token::IntSuffixed(_)
                        | Token::XInt(_)
                        | Token::XIntSuffixed(_)
                        | Token::Float64(_)
                        | Token::Float32(_)
                        | Token::XIEEE64(_)
                        | Token::XIEEE32(_)
                        | Token::Decimal(_)
                        | Token::BigNum(_)
                )),
                _
            )),
        )
    }

    /// `expr<typeArgs>` → [`SyntaxKind::TYPE_APP_EXPR`] (FCS `SynExpr.TypeApp`).
    /// The head sits at `cp` (FCS's `atomicExpr`); the `< … >` block reuses the
    /// type-side type-argument machinery ([`Parser::parse_type_arg_actual`] +
    /// the comma loop), exactly as the prefix [`SyntaxKind::APP_TYPE`] does.
    ///
    /// The `HIGH_PRECEDENCE_TYAPP` adjacency virtual is consumed zero-width as
    /// an `ERROR` (mirroring `HighPrecedenceParenApp`'s treatment in
    /// [`Self::parse_postfix_tail`] / the prefix `APP_TYPE` wrap). The spaced
    /// empty form `f< >` (FCS's `typeArgsActual: LESS GREATER` arm) yields zero
    /// args with no error.
    ///
    /// The arg list and the close `>` reuse the exact shape of the prefix
    /// `APP_TYPE` consumer in [`Parser::parse_atomic_type`]. The close `>` is
    /// consumed if present and otherwise left in place (no diagnostic of our
    /// own): the LexFilter only emits `HighPrecedenceTyApp` when its balance
    /// walk has already found the matching `>` (`peekAdjacentTypars`), so an
    /// unclosed `f<int` never reaches here — it stays a comparison `f < int`,
    /// markerless. The `else` is therefore defensive; a stray unconsumed `>`
    /// flows to the enclosing context's own recovery, keeping the round-trip
    /// lossless and the parser panic-free.
    fn parse_type_app_tail(&mut self, cp: rowan::Checkpoint) {
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TYPE_APP_EXPR));
        // HPA virtual: consume as zero-width ERROR.
        self.bump_into(SyntaxKind::ERROR);
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Less(_))), _))
        ) {
            self.bump_into(SyntaxKind::LESS_TOK);
        }
        // Skip the arg loop for the empty `< >` form (the close `>` is already
        // next); otherwise parse a comma-separated `typeArgActual` list.
        if !matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Greater(_))), _)),
        ) {
            self.parse_type_arg_actual();
            while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Comma)), _))) {
                self.bump_into(SyntaxKind::COMMA_TOK);
                self.parse_type_arg_actual();
            }
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Greater(_))), _)),
        ) {
            self.bump_into(SyntaxKind::GREATER_TOK);
        }
        self.builder.finish_node();
    }

    /// `expr.[index]` → [`SyntaxKind::DOT_INDEXED_GET_EXPR`] (FCS
    /// `DotIndexedGet`). The head sits at `cp` (FCS's `objectExpr`); the
    /// bracketed expression is `indexArgs` (a `Tuple` for `arr.[i, j]`). The
    /// index is parsed as a full `parse_expr`; a range/slice index
    /// (`arr.[1..3]`) is deferred and errors at the `..`. The closer `]` is a
    /// real `RBRACK_TOK` — unlike `)`, the lex-filter does not swallow it.
    fn parse_dot_indexed_get_tail(&mut self, cp: rowan::Checkpoint) {
        self.builder.start_node_at(
            cp,
            FSharpLang::kind_to_raw(SyntaxKind::DOT_INDEXED_GET_EXPR),
        );
        self.bump_into(SyntaxKind::DOT_TOK);
        self.bump_into(SyntaxKind::LBRACK_TOK);
        // The index may be a from-end bound (`arr.[^1]`, whose `^` is an ordinary
        // expression start) or an open-lower slice (`arr.[..^1]`, lex-filter-split
        // to `..` then `^1`, both ordinary expression starts), so the plain
        // expression-start gate covers them.
        if self.peek_is_expr_start() {
            self.parse_expr();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected an index expression after `.[`".to_string(),
                span,
            });
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::RBrack)), _))
        ) {
            self.bump_into(SyntaxKind::RBRACK_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `]` to close the indexer".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// `a?b` → [`SyntaxKind::DYNAMIC_EXPR`] (FCS `Dynamic`). The head (the LHS
    /// `funcExpr`) sits at `cp`; the `dynamicArg` after the `?` is either a single
    /// identifier (emitted as an [`SyntaxKind::IDENT_EXPR`], FCS's
    /// `SynExpr.Ident` — the dynamic member name, *one* segment only, so a
    /// following `.member` chains onto the whole `Dynamic` via the outer tail
    /// loop) or a parenthesised expression (`a?(e)` → the same
    /// [`SyntaxKind::PAREN_EXPR`] [`Self::parse_paren_expr`] builds). The caller
    /// has verified the cursor is at the `?`.
    fn parse_dynamic_tail(&mut self, cp: rowan::Checkpoint) {
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::DYNAMIC_EXPR));
        self.bump_into(SyntaxKind::QMARK_TOK);
        // The `dynamicArg` must be the next *raw* token. For incomplete input
        // inside a swallowed delimiter — `(a?)b`, `a?()b` — the LexFilter has
        // removed the `)` from the filtered stream, so the cursor would otherwise
        // sit on the *outside* token (`b`) and wrongly drag it into the
        // `DYNAMIC_EXPR` (draining the real `)` as ERROR). Both are FCS errors;
        // bail with a clean recovery error so the enclosing construct claims the
        // closer. A valid `a?b` / `a ? b` / `a?(e)` is always raw-adjacent here —
        // only intervening trivia separates the `?` from its argument.
        if !self.cursor_is_raw_adjacent() {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected an identifier or `( … )` after `?`".to_string(),
                span,
            });
            self.builder.finish_node();
            return;
        }
        match self.peek().cloned() {
            // `( e )` — the parenthesised `dynamicArg`. FCS's `dynamicArg` is
            // `LPAREN typedSequentialExpr rparen`, a **non-empty** body, so the
            // raw token after the `(` must start an expression and must not be the
            // `)` of an empty `a?()` (an FCS error) nor reached across a swallowed
            // closer. The atomic dispatcher's `(`-arm makes the same peek;
            // checking it here (rather than calling `parse_paren_expr`
            // unconditionally) stops the empty/incomplete `a?()b` from letting
            // `parse_paren_expr` drag the outside `b` in past the swallowed `)`.
            // `Hash` (`a?(#…#)` inline IL) is likewise not a `dynamicArg`.
            Some((Ok(FilteredToken::Raw(Token::LParen)), lparen_span)) => {
                match self.next_non_trivia_raw_after(lparen_span.end) {
                    Some(t)
                        if raw_after_lparen_starts_expr(t)
                            && !matches!(t, Token::RParen | Token::Hash) =>
                    {
                        // A genuine non-empty paren body — the shared paren path
                        // with the trait-call form disabled (FCS's `dynamicArg`
                        // `(` body is a bare `typedSequentialExpr`, so a trait call
                        // there is a parse error unless nested as `a?((…))`).
                        self.parse_paren_expr_impl(false);
                    }
                    _ => {
                        // Empty (`a?()`), inline IL, or incomplete — an FCS error.
                        // Consume the `(` for losslessness and report; the
                        // swallowed `)` and any following token are left to the
                        // enclosing construct.
                        let span = lparen_span.clone();
                        self.errors.push(ParseError {
                            message: "expected an expression in the `( … )` after `?`".to_string(),
                            span,
                        });
                        self.bump_into(SyntaxKind::LPAREN_TOK);
                    }
                }
            }
            // A single identifier `dynamicArg` — FCS's `SynExpr.Ident`. Emit a
            // bare `IDENT_EXPR` (one segment); a trailing `.member` is *not*
            // folded here — it chains onto the whole `Dynamic` in the outer loop
            // (`a?b.c` = `DotGet(Dynamic(a, b), [c])`).
            Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), _)) => {
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::IDENT_EXPR));
                self.bump_into(SyntaxKind::IDENT_TOK);
                self.builder.finish_node();
            }
            other => {
                let span = other
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected an identifier or `( … )` after `?`".to_string(),
                    span,
                });
            }
        }
        self.builder.finish_node();
    }

    /// `expr.member(.member)*` → [`SyntaxKind::DOT_GET_EXPR`] (FCS `DotGet`).
    /// The head sits at `cp` (FCS's `expr`); the member path is a
    /// [`SyntaxKind::LONG_IDENT`] child holding the leading `.` and the member
    /// idents. Consecutive `.member`s fold into the one `LONG_IDENT`, matching
    /// `mkSynDot`'s `DotGet` append arm (`(f x).Bar.Baz` →
    /// `DotGet(Paren …, ["Bar"; "Baz"])`). A trailing operator-value segment
    /// `.( op )` / `.(*)` folds in the same way (`(id 1).(+)` →
    /// `DotGet(Paren …, ["+"])`). The loop stops at a `.[` indexer, leaving it
    /// for the outer tail loop.
    ///
    /// Each segment's `.` must be the next *raw* significant token, so the
    /// chain never folds a member that lives past a LexFilter-swallowed closer:
    /// in `((f y).Bar).Baz` the inner `DotGet` is parenthesised, so a swallowed
    /// `)` sits between `.Bar` and the outer `.Baz` — without the guard the
    /// inner chain would swallow `.Baz` and drain the `)` as an `ERROR`. Same
    /// raw-adjacency check as [`Self::parse_postfix_dot_tail`] /
    /// [`Self::parse_ident_expr`].
    fn parse_dot_get_tail(&mut self, cp: rowan::Checkpoint) {
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::DOT_GET_EXPR));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        // Each segment is a raw-adjacent `.` (not across a swallowed closer)
        // followed by an ident or an operator-value `( op )` / `(*)` / `( * )`.
        // A `.[` indexer (excluded by `at_long_ident_segment`) or any other
        // trailing dot ends the chain, left for the outer
        // `parse_postfix_dot_tail` loop.
        self.consume_long_ident_qualification_segments();
        self.builder.finish_node(); // LONG_IDENT
        self.builder.finish_node(); // DOT_GET_EXPR
    }

    /// `true` when a *general* parenthesised operator-value `( op )` (FCS's
    /// `opName: LPAREN operatorName rparen`, `pars.fsy:6794`) begins at the
    /// filtered position `lparen_pos`: that token is `(`, the next filtered
    /// token is a single-token operator name ([`is_paren_operator_name`]), and
    /// the operator is *immediately* followed by `)`.
    ///
    /// The `)` is consulted on the **raw** stream (LexFilter swallows it from
    /// the filtered stream, so `next_non_trivia_raw_after` is the only way to
    /// see it). The immediate-`)` requirement is what distinguishes the
    /// operator-value `(-)` / `(..)` from a prefix application `(- x)` or an
    /// open-ended range `(..3)`, whose operators are followed by an operand.
    ///
    /// The glued `(*)` multiply operator-value is a single
    /// [`Token::LParenStarRParen`] token, handled by its own dispatch arm — it
    /// is *not* matched here. The spaced `( * )` is **excluded** in this
    /// (bare-value) form — it is the whole-dimension wildcard — but **included**
    /// in dot-qualification position via [`Self::at_dot_paren_op_value`].
    pub(super) fn at_paren_op_value(&self, lparen_pos: usize) -> bool {
        self.paren_op_value_at(lparen_pos, false)
    }

    /// As [`Self::at_paren_op_value`], but for a `.( op )` segment in
    /// dot-qualification position, where the spaced `( * )` is *also* an
    /// operator-value. FCS parses `expr.( * )` through the
    /// `atomicExprQualification: LPAREN typedSequentialExpr rparen` arm
    /// (`pars.fsy:5354`), matching the body's `IndexRange(None, None)` wildcard
    /// and rewriting it to `op_Multiply` — so `Operators.( * )` /
    /// `(id 1).( * )` are valid where bare `( * )` is the wildcard.
    pub(super) fn at_dot_paren_op_value(&self, lparen_pos: usize) -> bool {
        self.paren_op_value_at(lparen_pos, true)
    }

    /// As [`Self::at_paren_op_value`], but for a `( op )` operator name in
    /// *pattern* position (a binding/match head). The spaced `( * )` is also an
    /// operator-value here: FCS's `operatorName` includes the bare `STAR`
    /// (`pars.fsy:6862`), and — unlike an expression — a pattern has no
    /// `IndexRange(None, None)` whole-dimension wildcard for `( * )` to collide
    /// with, so `let ( * ) a b = …` is unambiguously the multiply operator
    /// (matching FCS). Hence `allow_star = true`, identical to the
    /// dot-qualification case.
    pub(super) fn at_paren_op_value_pat(&self, lparen_pos: usize) -> bool {
        self.paren_op_value_at(lparen_pos, true)
    }

    /// Shared core of [`Self::at_paren_op_value`] /
    /// [`Self::at_dot_paren_op_value`] / [`Self::at_paren_op_value_pat`].
    /// `allow_star` admits the lone `Op("*")` as an operator name (true in
    /// dot-qualification and pattern position, false at a bare expression atom
    /// where `( * )` is the whole-dimension wildcard).
    fn paren_op_value_at(&self, lparen_pos: usize, allow_star: bool) -> bool {
        if !matches!(
            self.filtered_tokens.get(lparen_pos),
            Some((Ok(FilteredToken::Raw(Token::LParen)), _))
        ) {
            return false;
        }
        let Some((Ok(FilteredToken::Raw(op)), op_span)) = self.filtered_tokens.get(lparen_pos + 1)
        else {
            return false;
        };
        if !(is_paren_operator_name(op) || (allow_star && matches!(op, Token::Op("*")))) {
            return false;
        }
        matches!(
            self.next_non_trivia_raw_after(op_span.end),
            Some(Token::RParen)
        )
    }

    /// Parse a bare parenthesised operator-value as an atomic expression →
    /// `LONG_IDENT_EXPR > LONG_IDENT > [LPAREN_TOK, IDENT_TOK(op), RPAREN_TOK]`,
    /// matching FCS's `identExpr: opName` (`pars.fsy:6962`), which builds a
    /// single-segment `SynExpr.LongIdent` whose ident is the mangled `op_*`
    /// name and whose trivia is `OriginalNotationWithParen <op>`. Our green
    /// tree stores the raw operator token directly under `IDENT_TOK`; the
    /// FCS-side differential normaliser unwraps the trivia to the same source
    /// spelling. The caller has verified [`Self::at_paren_op_value`].
    ///
    /// When `fold` is set, a trailing `.member` / `.(op)` qualification folds
    /// onto the same `LONG_IDENT` (FCS's `mkSynDot` appends to the
    /// `SynExpr.LongIdent` an `opName` produces, so `(+).GetType` is the single
    /// long-ident `["op_Addition"; "GetType"]`, *not* a `DotGet`), leaving any
    /// `.[`/dangling dot for the postfix tail. `fold` is `false` in the
    /// head-only HPA-argument path (see [`Self::parse_atomic_expr`]).
    pub(super) fn parse_paren_op_value_expr(&mut self, fold: bool) {
        let cp = self.builder.checkpoint();
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_EXPR));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        self.consume_paren_op_value();
        if fold {
            self.consume_long_ident_qualification_segments();
        }
        self.builder.finish_node(); // LONG_IDENT
        self.builder.finish_node(); // LONG_IDENT_EXPR
    }

    /// Parse a bare active-pattern-name value as an atomic expression →
    /// `LONG_IDENT_EXPR > LONG_IDENT > [ACTIVE_PAT_NAME, …]`, matching FCS's
    /// `identExpr: opName` (`pars.fsy:6962`) for the active-pattern productions
    /// (`pars.fsy:6812-6819`). FCS builds a single-segment `SynExpr.LongIdent`
    /// whose ident is the whole name (total `"|Foo|Bar|"`, partial `"|Foo|_|"`)
    /// with `IdentTrivia.HasParenthesis` — *structurally identical* to a
    /// parenthesised operator-value, differing only in trivia. We store the name
    /// as an [`SyntaxKind::ACTIVE_PAT_NAME`] segment under the `LONG_IDENT` (the
    /// same node the pattern-position name uses); the differential normaliser
    /// rebuilds FCS's `idText` from its case tokens. The caller has verified
    /// [`Self::at_active_pat_name`].
    ///
    /// `fold` matches [`Self::parse_paren_op_value_expr`]: when set, a trailing
    /// `.member` / `.(op)` / `.(|…|)` qualification folds onto the same
    /// `LONG_IDENT` (FCS's `mkSynDot` append — `(|Foo|_|).Bar` is the single
    /// `LongIdent(["|Foo|_|"; "Bar"])`, not a `DotGet`). `fold` is `false` in
    /// the head-only HPA-argument path (see [`Self::parse_atomic_expr`]).
    pub(super) fn parse_active_pat_name_expr(&mut self, fold: bool) {
        let cp = self.builder.checkpoint();
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_EXPR));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        // Emits the `ACTIVE_PAT_NAME` node (shared with pattern position) as a
        // child of the open `LONG_IDENT`.
        self.parse_active_pat_name();
        if fold {
            self.consume_long_ident_qualification_segments();
        }
        self.builder.finish_node(); // LONG_IDENT
        self.builder.finish_node(); // LONG_IDENT_EXPR
    }

    /// Emit the three tokens of a general `( op )` operator-value
    /// (`[LPAREN_TOK, IDENT_TOK(op), RPAREN_TOK]`) into the currently-open
    /// node. Shared by the bare-value [`Self::parse_paren_op_value_expr`] and
    /// the qualified `path.(op)` / `expr.(op)` segment loops, which open the
    /// enclosing `LONG_IDENT` themselves. The closing `)` is LexFilter-swallowed
    /// (recovered from the raw stream via [`Self::bump_swallowed_rparen`], like
    /// [`Self::parse_paren_expr`]). The caller has verified
    /// [`Self::at_paren_op_value`], so the operator and a closing `)` are
    /// present. The operator token (`Op`, `Equals`, `Less`, …) is bumped into
    /// an `IDENT_TOK`, the same encoding the infix/prefix paths use
    /// ([`Parser::emit_infix_op_as_long_ident`]).
    pub(super) fn consume_paren_op_value(&mut self) {
        self.bump_into(SyntaxKind::LPAREN_TOK);
        self.bump_into(SyntaxKind::IDENT_TOK);
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
    }

    /// Emit a glued `(*)` operator-value (the single [`Token::LParenStarRParen`]
    /// lexer token) as the three tokens `[LPAREN_TOK, IDENT_TOK("*"),
    /// RPAREN_TOK]` into the currently-open node, so the operator reads as `*`
    /// (FCS's `op_Multiply`, `pars.fsy:6804`) rather than the literal `(*)`.
    /// The token spans exactly three ASCII bytes `( * )`, so the sub-spans are
    /// `[s, s+1)`, `[s+1, s+2)`, `[s+2, s+3)`. Leading trivia is drained first
    /// (mirroring [`Self::bump_into`]); the filtered + raw cursors then advance
    /// past the one token.
    pub(super) fn consume_star_op_value(&mut self) {
        let (_, span) = self
            .filtered_tokens
            .get(self.pos)
            .cloned()
            .expect("consume_star_op_value entered without a token");
        debug_assert!(span.end - span.start == 3, "`(*)` token spans 3 bytes");
        self.drain_raw_up_to(span.start);
        let s = span.start;
        self.emit_text(SyntaxKind::LPAREN_TOK, s..s + 1);
        self.emit_text(SyntaxKind::IDENT_TOK, s + 1..s + 2);
        self.emit_text(SyntaxKind::RPAREN_TOK, s + 2..s + 3);
        self.pos += 1;
        while let Some((_, raw_span)) = self.raw_tokens.get(self.raw_pos) {
            if raw_span.end <= span.end {
                self.raw_pos += 1;
            } else {
                break;
            }
        }
    }

    /// Parse a bare glued `(*)` multiply operator-value as an atomic expression
    /// → `LONG_IDENT_EXPR > LONG_IDENT > [LPAREN_TOK, IDENT_TOK("*"),
    /// RPAREN_TOK]`. The caller has verified the cursor is at
    /// [`Token::LParenStarRParen`]. `fold` matches
    /// [`Self::parse_paren_op_value_expr`]: when set, a trailing `.member` /
    /// `.(op)` folds onto the same long-ident.
    pub(super) fn parse_star_op_value_expr(&mut self, fold: bool) {
        let cp = self.builder.checkpoint();
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_EXPR));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        self.consume_star_op_value();
        if fold {
            self.consume_long_ident_qualification_segments();
        }
        self.builder.finish_node(); // LONG_IDENT
        self.builder.finish_node(); // LONG_IDENT_EXPR
    }

    /// Consume a run of raw-adjacent long-ident qualification segments —
    /// `.ident`, `.( op )`, `.(*)`, or `.( * )` — into the currently-open
    /// `LONG_IDENT` node (FCS's `mkSynDot` append). Stops (without consuming
    /// the `.`) at a `.[` indexer, a swallowed-closer-crossing dot, or a dot
    /// followed by none of those, leaving it for the caller / the postfix
    /// tail. The spaced `( * )` *is* an operator-value here (dot-qualification
    /// position — see [`Self::at_dot_paren_op_value`]). Shared by the bare
    /// operator-value heads and [`Self::parse_dot_get_tail`].
    fn consume_long_ident_qualification_segments(&mut self) {
        while self.at_long_ident_segment() {
            match self.filtered_tokens.get(self.pos + 1).cloned() {
                Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), _)) => {
                    self.bump_into(SyntaxKind::DOT_TOK);
                    self.bump_into(SyntaxKind::IDENT_TOK);
                }
                Some((Ok(FilteredToken::Raw(Token::LParenStarRParen)), _)) => {
                    self.bump_into(SyntaxKind::DOT_TOK);
                    self.consume_star_op_value();
                }
                Some((Ok(FilteredToken::Raw(Token::LParen)), _))
                    if self.at_dot_paren_op_value(self.pos + 1) =>
                {
                    self.bump_into(SyntaxKind::DOT_TOK);
                    self.consume_paren_op_value();
                }
                // `.(|Foo|_|)` — an active-pattern-name qualification segment
                // (`opName` folded onto the path, FCS's `mkSynDot`). The bare
                // `|` after the segment's `(` is detected one token ahead, like
                // the operator-value arm above.
                Some((Ok(FilteredToken::Raw(Token::LParen)), _))
                    if self.at_active_pat_name_at(self.pos + 1) =>
                {
                    self.bump_into(SyntaxKind::DOT_TOK);
                    self.parse_active_pat_name();
                }
                _ => break,
            }
        }
    }

    /// `SynExpr.Paren` — a parenthesised expression `( e )`. The caller
    /// (`parse_expr`) has already ensured the `LParen` is followed by an
    /// expression-starter (not `RParen` — that's unit, handled by
    /// [`Parser::parse_const_expr`]). Shape:
    /// `PAREN_EXPR > [LPAREN_TOK, <inner-expr>, RPAREN_TOK]` with any
    /// trivia between the parens flowing in as interleaved children.
    ///
    /// The closing `RParen` is "swallowed" by the lexfilter (it never
    /// reaches the filtered stream) so we read it directly off the raw
    /// stream via [`Parser::bump_swallowed_rparen`], the same path used
    /// by the unit-literal arm.
    pub(super) fn parse_paren_expr(&mut self) {
        // The atomic-head `( … )` is a full `parenExprBody`, so the SRTP
        // trait-call form is admitted here.
        self.parse_paren_expr_impl(true);
    }

    /// Shared body of [`Self::parse_paren_expr`]. `allow_trait_call` selects
    /// between FCS's `parenExprBody` (`true` — the atomic-head paren, which may
    /// be an SRTP trait call `( ^a : (static member …) x )`) and the bare
    /// `typedSequentialExpr` paren (`false` — the dynamic-lookup argument
    /// `a?( e )`, whose `dynamicArg` grammar is `LPAREN typedSequentialExpr
    /// rparen`, *not* a `parenExprBody`, so a trait-call body there is an FCS
    /// error — `a?(^T : (static member …) x)` must be nested as `a?((…))`).
    fn parse_paren_expr_impl(&mut self, allow_trait_call: bool) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::PAREN_EXPR));
        self.bump_into(SyntaxKind::LPAREN_TOK);
        // Drain raw trivia between `(` and the inner expression so it
        // attaches to `PAREN_EXPR` rather than landing inside the inner
        // expr's opening node (where `parse_expr`'s downstream
        // `start_node` would put it). Symmetric to the trailing trivia,
        // which `bump_swallowed_rparen` drains under `PAREN_EXPR`.
        if let Some((_, next_span)) = self.peek() {
            let start = next_span.start;
            self.drain_raw_up_to(start);
        }
        // An SRTP trait-call expression `( ^a : (static member M : sig) arg )`
        // (FCS's `SynExpr.TraitCall`, `pars.fsy:5529`). Detected before the
        // expression gate because the leading `^a` head typar is *not* an
        // expression start (`^` is an operator), so `peek_is_expr_start` would
        // reject it. A leading head typar inside parens has no other parse, so
        // committing here is faithful — the closing-`)`/argument scaffolding is
        // FCS's `typars COLON LPAREN classMemberSpfn rparen typedSequentialExpr`.
        // Skipped for the dynamic-argument paren (a bare `typedSequentialExpr`),
        // where the trait-call form is an FCS error left to the expression gate.
        if allow_trait_call && self.at_trait_call_body() {
            self.parse_trait_call_expr();
            self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
            self.builder.finish_node();
            return;
        }
        // A *head typar* `(^a …)` that is not a complete supported trait-call is
        // FCS's incomplete-SRTP-trait-call form, which it reserves and errors on
        // (`(^f)`, `(^f : int)`, `arr.[(^i)]`) — it is **not** a from-end `^expr`
        // (that is `(^1)` / `arr.[^i]`, where the `^` operand is not a typar). Now
        // that `^` is a general expression start, guard against mis-accepting these
        // as `IndexFromEnd`: record the error and drain the paren body, matching
        // FCS rather than producing a non-FCS tree. (`consume_trait_call_support`
        // distinguishes the typar `^ident` from a from-end `^<literal>`.)
        if allow_trait_call && self.at_paren_head_typar() {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "a parenthesised `^` head type-parameter starts a \
                          statically-resolved trait call; expected `: (member …)`"
                    .to_string(),
                span,
            });
            // Drain the body losslessly up to the LexFilter-swallowed `)`.
            while self.peek().is_some() && !self.at_swallowed_seq_closer() {
                self.bump_into(SyntaxKind::ERROR);
            }
            self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
            self.builder.finish_node();
            return;
        }
        // The outer `peek_is_expr_start` validated *this* LParen's
        // lookahead, but the inner position needs its own validation —
        // nested `((` or `((+))` have an inner non-expression-starter
        // and `parse_expr`'s downstream `parse_const_expr` would hit
        // its own `unreachable!`. Recover with an error rather than
        // recursing on unparseable input.
        if self.peek_is_expr_start() {
            // FCS's `parenExprBody` is a full `typedSequentialExpr`
            // (`pars.fsy:5531`), so the body sequences *and* absorbs a trailing
            // `: T` annotation: `(a; b)` is `Paren(Sequential(a, b))` and
            // `(a; b : int)` is `Paren(Typed(Sequential(a, b), int))`. Both are
            // handled by the shared seq-block gatherer — which accepts the `;`
            // (`Token::Semi`) and offside `Virtual::BlockSep` separators, stops
            // at the LexFilter-swallowed `)` boundary, and wraps the trailing
            // `: T` in `TYPED_EXPR` itself (the `typedSequentialExpr` production,
            // shared with lambda / `if` / `try` bodies). The outer gate above
            // guarantees the first statement is present, so the gatherer's
            // missing-first diagnostic never fires here. Outer-typed `(e) : T`
            // (the `:` *past* the swallowed `)`) is left for the enclosing
            // context by the gatherer's raw-stream gate.
            self.parse_seq_block_body("expected expression inside parentheses");
        } else {
            let span = self
                .peek()
                .map(|(_, span)| span.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected expression inside parentheses".to_string(),
                span,
            });
        }
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        self.builder.finish_node();
    }

    /// `begin … end` — the verbose-syntax parenthesis (`beginEndExpr`,
    /// `pars.fsy:5419`). Unlike `( … )`, both delimiters are *real* filtered
    /// tokens (the lexfilter passes `begin`/`end` through, treating `begin` as a
    /// `TokenLExprParen` so the inner body is offside-suppressed exactly like a
    /// paren). The caller verified the cursor is at `Token::Begin`.
    ///
    /// * `begin end` (empty body) → `SynConst.Unit` (FCS's `mkSynUnit`,
    ///   `pars.fsy:5430`): a [`SyntaxKind::CONST_EXPR`] holding just the
    ///   `BEGIN_TOK`/`END_TOK` pair, projected to unit by the normaliser (the
    ///   `BEGIN_TOK` literal kind, mirroring the `LPAREN_TOK` unit form).
    /// * `begin e end` → `SynExpr.Paren e` (`pars.fsy:5421`): a
    ///   [`SyntaxKind::PAREN_EXPR`] wrapping the inner `typedSequentialExpr`,
    ///   the same body gatherer the `( … )` path uses.
    fn parse_begin_end_expr(&mut self) {
        // Empty `begin end` → unit. The next significant filtered token being
        // `end` (no virtual between — `begin` is `TokenLExprParen`, so the empty
        // body pushes no offside scaffolding) is the discriminator.
        let empty = matches!(
            self.next_non_trivia_filtered_after_pos(),
            Some(FilteredToken::Raw(Token::End))
        );
        let kind = if empty {
            SyntaxKind::CONST_EXPR
        } else {
            SyntaxKind::PAREN_EXPR
        };
        self.builder.start_node(FSharpLang::kind_to_raw(kind));
        self.bump_into(SyntaxKind::BEGIN_TOK);
        if !empty {
            // Drain trivia between `begin` and the inner expression so it
            // attaches to the node rather than the inner expr's opening node
            // (symmetric to `parse_paren_expr_impl`).
            if let Some((_, next_span)) = self.peek() {
                let start = next_span.start;
                self.drain_raw_up_to(start);
            }
            // The body is FCS's `typedSequentialExpr` — the shared seq-block
            // gatherer, which stops at the real `end` (not an expr-start, not a
            // separator). `at_swallowed_seq_closer` only guards the swallowed
            // `)`/`}`, so the real `end` never confuses the separator loop.
            self.parse_seq_block_body("expected expression inside `begin … end`");
        }
        // The closing `end` — a real filtered token (FCS's `END`). Its leading
        // trivia is drained under this node by `bump_into`. A missing `end` is
        // FCS's `BEGIN … recover` (`parsUnmatchedBegin`); record a clean error
        // and leave the cursor for the enclosing construct.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::End)), _))) {
            self.bump_into(SyntaxKind::END_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `end` to close the `begin … end` block".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// `true` iff the cursor (just past a paren expression's opening `(`) is at
    /// the trait-call commit point `typars COLON LPAREN` (`pars.fsy:5529`) *and*
    /// the member signature's name is one we parse faithfully.
    ///
    /// The support is FCS's `typars` — either a bare typar or the parenthesised
    /// alternatives `( ^a or … )`, both consumed by
    /// [`Self::consume_trait_call_support`]. As a *bare* support FCS admits only
    /// the head-type typar `^a`: the plain `'a` form `( 'a : (static member …) x)`
    /// is a parse error ("Unexpected keyword 'static' in binding"), so that arm
    /// fires on the `^` sigil (`Token::Op("^")`), not `Token::Quote` — inside the
    /// alternatives, where `typarAlts`' base case is the full `typar`, `'a` *is*
    /// admitted. The member signature's opening `(` (after the `:`) is the next
    /// significant raw token past the support; its closing `)` is
    /// LexFilter-swallowed and so is *not* peeked here. Neither a head typar nor a
    /// paren-headed typar list has any other parse inside a paren expression, so
    /// this lookahead is a sound commit.
    ///
    /// The member signature must be one [`Self::parse_member_sig`] parses
    /// faithfully, checked by the shared [`Self::member_sig_body_is_supported`]
    /// (also the SRTP member-*constraint* gate): a `static`/`abstract`/`member`
    /// introducer (FCS's `memberSpecFlags`, required — a bare `(M : …)` is an FCS
    /// error) then an identifier name, or a `new` ctor. An operator name, an
    /// `inline`, or a leading access modifier — all valid `classMemberSpfn` that
    /// `parse_member_sig` does not yet consume — make the check `false`, so the
    /// trait-call branch defers them to a clean error rather than committing and
    /// misparsing (e.g. claiming a `(+)` operator paren as the member-sig closer).
    /// `true` when the paren body at the cursor is the *incomplete* SRTP
    /// trait-call form FCS reserves and errors on — a head typar support (`^a`,
    /// or alternatives `(^a or ^b …)`) immediately followed by `)` (`(^a)`) or `:`
    /// (`(^a : …)`). It is **not** triggered by a from-end expression that merely
    /// *starts* with `^ident` and then continues (`(^a.b)`, `(^a + 1)`), nor by a
    /// from-end `^<literal>` (whose operand is not a typar). Looser than
    /// [`Self::at_trait_call_body`] (which requires the full `: (member …)`) but
    /// tighter than a bare head-typar prefix, so only the genuinely incomplete
    /// SRTP shapes are diverted to recovery; everything else stays a from-end expr.
    fn at_paren_head_typar(&self) -> bool {
        let mut sig = self.significant_raw_from_cursor();
        Self::consume_trait_call_support(&mut sig)
            && matches!(sig.next(), Some(Token::RParen | Token::Colon))
    }

    fn at_trait_call_body(&self) -> bool {
        let mut sig = self.significant_raw_from_cursor();
        // The `typars` support — a single head typar `^a`, or the parenthesised
        // typar-alternatives `( ^a or ^b … )` (FCS's `typars`, `pars.fsy:5535`).
        if !Self::consume_trait_call_support(&mut sig) {
            return false;
        }
        // Then `COLON LPAREN` and a member-sig body the parser handles.
        if !matches!(sig.next(), Some(Token::Colon)) || !matches!(sig.next(), Some(Token::LParen)) {
            return false;
        }
        Self::member_sig_body_is_supported(&mut sig)
    }

    /// Consume the `typars` support prefix of a trait call from the significant
    /// raw token iterator, leaving it positioned just after (at the `:`). Returns
    /// `false` (without a well-formed consume) when the cursor is not a trait-call
    /// support. Two shapes (FCS's `typars`, `pars.fsy:5535`): a single head typar
    /// `^a`, or the parenthesised alternatives `( ^a or … )`
    /// (`LPAREN typarAlts rparen`).
    ///
    /// `typarAlts` (`pars.fsy:5546`) is `typar (OR appTypeCanBeNullable)*`: the
    /// *first* alternative is a typar — `^a` or, since the base case is the full
    /// `typar`, the plain `'a` (which FCS accepts here even though the bare
    /// single support `('a : (static member …) x)` is a parse error) — and every
    /// *later* alternative is a whole `appType`, concrete types included
    /// (`(^T or int)`, `(^T or int list)`). So the later alternatives cannot be
    /// matched token-by-token; they are skipped with a bracket-depth scan up to
    /// the alternatives' own closing `)`, which is the first `)` at depth zero. A
    /// depth-zero `or` separates them; anything else is part of the current
    /// alternative and is left to [`Parser::parse_type_alt_operand`] to parse (or
    /// reject) — the commit itself is already sound, since a `(`-headed typar has
    /// no other parse in this position.
    fn consume_trait_call_support<'a>(sig: &mut impl Iterator<Item = &'a Token<'src>>) -> bool
    where
        'src: 'a,
    {
        match sig.next() {
            // Single head typar `^a`.
            Some(Token::Op("^")) => {
                matches!(sig.next(), Some(Token::Ident(_) | Token::QuotedIdent(_)))
            }
            // `( ^a or … )` — alternatives. The first must be a typar.
            Some(Token::LParen) => {
                // The sigil decides whether the *singleton* `( typar )` is legal.
                // FCS accepts `((^T) : (static member …) …)` but rejects
                // `(('T) : …)` — the quoted typar reaches a trait-call support only
                // as the base of a real alternatives list (`(('T or int) : …)`,
                // which FCS does accept). So a `'`-headed support must be followed
                // by an `or`; a `^`-headed one may close immediately.
                let quoted = match sig.next() {
                    Some(Token::Op("^")) => false,
                    Some(Token::Quote) => true,
                    _ => return false,
                };
                if !matches!(sig.next(), Some(Token::Ident(_) | Token::QuotedIdent(_))) {
                    return false;
                }
                Self::consume_type_alts_tail(sig, quoted)
            }
            _ => false,
        }
    }

    /// Scan the `(OR appType)*` tail of a trait call's parenthesised `typarAlts`,
    /// with the iterator positioned just past the first (typar) alternative, and
    /// consume the alternatives' closing `)`. Returns `false` when the shape is
    /// not a well-formed alternatives list: a token where the separator or closer
    /// is due (`(^a b)`), an empty alternative (`(^a or )`, `(^a or or b)`), an
    /// unterminated list, or — when `requires_or` — a singleton with no `or` at
    /// all (`(('T) : …)`, which FCS rejects even though it accepts `((^T) : …)`).
    ///
    /// A *later* alternative is an `appType`, so it spans an unbounded run of
    /// tokens and may nest brackets (`(^a or (int * string))`, `(^a or int[])`).
    /// Only the *depth-zero* `or` / `)` are the list's own, so parens and brackets
    /// are depth-counted and every other token belongs to the alternative under
    /// way — the token scan cannot tell where an alternative *ends*, only that it
    /// is non-empty. That is enough for the commit (a paren-headed typar has no
    /// other parse here); rejecting an ill-formed alternative is
    /// [`Parser::parse_type_alt_operand`]'s job, as it is on the constraint side.
    fn consume_type_alts_tail<'a>(
        sig: &mut impl Iterator<Item = &'a Token<'src>>,
        requires_or: bool,
    ) -> bool
    where
        'src: 'a,
    {
        // The first alternative is a *typar*, whose extent is known, so directly
        // after it only the separator or the list's closer is legal. `requires_or`
        // (a `'a`-sigil first typar) rules the closer out: FCS rejects the
        // singleton `(('T) : …)` while accepting `((^T) : …)`.
        match sig.next() {
            Some(Token::RParen) => return !requires_or,
            Some(Token::Or) => {}
            _ => return false,
        }
        let mut depth = 0usize;
        // Tokens in the alternative under way; zero directly after an `or`, so an
        // empty alternative is rejected rather than committed to.
        let mut alt_tokens = 0usize;
        loop {
            match sig.next() {
                None => return false,
                Some(Token::LParen | Token::LBrack) => {
                    depth += 1;
                    alt_tokens += 1;
                }
                Some(Token::RParen | Token::RBrack) if depth > 0 => {
                    depth -= 1;
                    alt_tokens += 1;
                }
                // The alternatives' own `)`: legal only after a non-empty alternative.
                Some(Token::RParen) => return alt_tokens > 0,
                // Bracket underflow — not this list's shape.
                Some(Token::RBrack) => return false,
                Some(Token::Or) if depth == 0 => {
                    if alt_tokens == 0 {
                        return false;
                    }
                    alt_tokens = 0;
                }
                Some(_) => alt_tokens += 1,
            }
        }
    }

    /// `pars.fsy:5529 parenExprBody` alt
    /// `typars COLON LPAREN classMemberSpfn rparen typedSequentialExpr` — the
    /// body of an SRTP trait call `( ^a : (static member M : sig) arg )`,
    /// emitting `SynExpr.TraitCall`. The caller ([`Self::parse_paren_expr`]) owns
    /// the surrounding `PAREN_EXPR`/`(`/`)` (FCS reaches a trait call only
    /// through `parenExpr: LPAREN parenExprBody rparen`, so the faithful shape is
    /// `Paren(TraitCall)`), and has verified the commit point via
    /// [`Self::at_trait_call_body`].
    ///
    /// Shape: `TRAIT_CALL_EXPR > [VAR_TYPE, COLON_TOK, LPAREN_TOK, MEMBER_SIG,
    /// RPAREN_TOK, <arg-expr>]`. The support type is the head-type typar `^a`
    /// (a [`SyntaxKind::VAR_TYPE`]); the member signature reuses the shared
    /// [`Self::parse_member_sig`] (`classMemberSpfn`, also the SRTP-constraint
    /// payload). The member sig's closing `)` is LexFilter-swallowed, so it is
    /// claimed from the raw stream via [`Self::bump_swallowed_closer`]; the
    /// argument is a `typedSequentialExpr` (the same body the normal paren path
    /// gathers), which stops at the swallowed *outer* `)` that the caller then
    /// claims.
    fn parse_trait_call_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TRAIT_CALL_EXPR));
        // The support — a single head-type typar `^a`, or the parenthesised
        // alternatives `( ^a or … )` (`at_trait_call_body` verified the shape).
        // The alternatives' *first* operand is a typar (`typarAlts`' base case) —
        // a `VAR_TYPE > [HAT_TOK | QUOTE_TOK, IDENT_TOK]` (`SynType.Var`); each
        // *later* one is a full `appTypeCanBeNullable`, so it goes through the
        // shared `parse_type_alt_operand` — with the *nullable* operand, unlike
        // the SRTP member *constraint*'s alternatives, whose `typeAlts` takes the
        // `appTypeWithoutNull`. (Same panic guard on an empty alternative.)
        // The alternatives' `)` is LexFilter-swallowed (like the member-sig
        // closer).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LParen)), _))
        ) {
            self.bump_into(SyntaxKind::LPAREN_TOK);
            let sigil = if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Quote)), _))) {
                SyntaxKind::QUOTE_TOK
            } else {
                SyntaxKind::HAT_TOK
            };
            self.parse_var_type(sigil);
            while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Or)), _))) {
                self.bump_into(SyntaxKind::OR_TOK);
                if !self.parse_type_alt_operand(TypeAltOperand::CanBeNullable) {
                    break;
                }
            }
            self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        } else {
            self.parse_var_type(SyntaxKind::HAT_TOK);
        }
        self.bump_into(SyntaxKind::COLON_TOK);
        self.bump_into(SyntaxKind::LPAREN_TOK);
        self.parse_member_sig();
        self.bump_swallowed_closer(
            SyntaxKind::RPAREN_TOK,
            |t| matches!(t, Token::RParen),
            ")",
            "trait-call member signature",
        );
        // The argument expression (`typedSequentialExpr`). The shared seq-block
        // gatherer stops at the LexFilter-swallowed outer `)`, which the caller
        // claims with `bump_swallowed_rparen`. Guard the filtered-stream
        // expression gate with the raw swallowed-closer check: when the argument
        // is absent and the outer `)` has been swallowed (`(^a : (… )) x`), the
        // filtered cursor already sits on the token *after* the trait call, so a
        // bare `peek_is_expr_start` would drag it in as the argument and drain the
        // real `)` as ERROR. FCS likewise rejects the missing-argument form.
        if self.peek_is_expr_start() && !self.at_swallowed_seq_closer() {
            self.parse_seq_block_body("expected an argument expression in a trait call");
        } else {
            let span = self
                .peek()
                .map(|(_, span)| span.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected an argument expression in a trait call".to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// `true` when the next non-trivia *raw* token is the inline-IL
    /// expression's LexFilter-swallowed outer `)`. The `)` that closes `(# … #)`
    /// is removed from the *filtered* stream (like every paren closer), so it
    /// survives only on the raw stream past `raw_consumed_end` —
    /// [`Self::next_non_trivia_raw_at_pos`] surfaces it where [`Self::peek`]
    /// cannot. [`Self::parse_inline_il_expr`] consults this before each optional
    /// continuation (arguments, `:` return type, closing `#`) so a malformed
    /// inline IL with a missing `#` (`(# "x") y`) cannot reach across the closer
    /// and steal the following token. Mirrors [`Self::bump_swallowed_rparen`]'s
    /// raw-stream view of the same `)`.
    pub(super) fn at_swallowed_inline_il_close(&self) -> bool {
        matches!(self.next_non_trivia_raw_at_pos(), Some(Token::RParen))
    }

    /// Skip any offside layout separators (`Virtual::BlockSep`) the lex-filter
    /// inserted between inline-IL tokens on a continuation line. Inside `(# … #)`
    /// the value arguments are space/newline-separated, not `;`-sequenced, so a
    /// block separator is spurious layout. It is a zero-width virtual, and the
    /// real newline/indent trivia is drained by the next token's `bump_into`, so
    /// dropping it (advancing only the filtered cursor) leaves the tree lossless
    /// *and* makes a multiline inline IL parse identically to its single-line
    /// form. Without this, `(# "neg"⏎ x : int #)` — whose continuation column
    /// makes LexFilter emit an `OBLOCKSEP` between the string and `x` — would
    /// stop at the separator and report a missing `#)`. (At other continuation
    /// columns no separator is emitted, so this is simply a no-op.)
    pub(super) fn skip_inline_il_layout(&mut self) {
        while matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
        ) {
            self.pos += 1;
        }
    }

    /// `pars.fsy:5640 inlineAssemblyExpr` (a `parenExprBody`) — the inline-IL
    /// expression `(# "instr" type (T) arg₀ … : retTy #)`
    /// (`SynExpr.LibraryOnlyILAssembly`, FSharp.Core only). The caller
    /// ([`Self::parse_atomic_expr_head`]) has verified the `(` is followed by a
    /// `#`. Because FCS reaches inline IL only through `parenExpr: LPAREN
    /// parenExprBody rparen`, this emits the FCS-faithful
    /// `Paren(LibraryOnlyILAssembly)` shape: `PAREN_EXPR > [LPAREN_TOK,
    /// INLINE_IL_EXPR > [HASH_TOK, <il-string>,
    /// (TYPE_TOK LPAREN_TOK <type> RPAREN_TOK)?, <arg-expr>*,
    /// (COLON_TOK (<type> | LPAREN_TOK RPAREN_TOK))?, HASH_TOK], RPAREN_TOK]`.
    ///
    /// Three things are LexFilter-swallowed (absent from the filtered stream)
    /// and recovered from the raw stream: the closing `)` (under the outer
    /// `PAREN_EXPR`) and the two inner `)`s — the `type (…)` paren and the
    /// `: ()` unit return — via [`Self::bump_swallowed_rparen`]; and the `type`
    /// keyword itself (its type-definition machinery swallows it), recovered
    /// like [`Self::parse_type_defn`]. The IL instruction string is a bare
    /// literal token (FCS hands it to `ParseAssemblyCodeInstructions`, it is not
    /// a `SynExpr`), so it is *not* wrapped in a `CONST_EXPR`.
    pub(super) fn parse_inline_il_expr(&mut self) {
        // FCS reaches inline IL only through `parenExpr: LPAREN parenExprBody
        // rparen` (`inlineAssemblyExpr` is a `parenExprBody`), so the faithful
        // shape is `SynExpr.Paren(SynExpr.LibraryOnlyILAssembly(…))`: the `(`/`)`
        // belong to an outer `PAREN_EXPR`, the `#…#` body to the inner
        // `INLINE_IL_EXPR`. This mirrors `(e : T)` →
        // `PAREN_EXPR > [LPAREN_TOK, TYPED_EXPR, RPAREN_TOK]`.
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::PAREN_EXPR));
        self.bump_into(SyntaxKind::LPAREN_TOK);
        // Drain any `(`–`#` trivia under `PAREN_EXPR` (mirrors `parse_paren_expr`)
        // so it doesn't land inside the inline-IL body.
        if let Some((_, next_span)) = self.peek() {
            let start = next_span.start;
            self.drain_raw_up_to(start);
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INLINE_IL_EXPR));
        // The opening `#`. Each step below first skips any offside layout
        // separators (`skip_inline_il_layout`) so a multiline inline IL parses
        // the same as its single-line form.
        self.bump_into(SyntaxKind::HASH_TOK);
        self.skip_inline_il_layout();
        self.parse_il_instruction_string();

        // Optional `type (T)` type argument (`opt_inlineAssemblyTypeArg`). The
        // `type` keyword is swallowed by LexFilter — it sits only in the raw
        // stream, with the filtered cursor already on the `(`. Claim it
        // directly (a `bump_into` would mark it ERROR), then the parenthesised
        // type whose `)` is itself swallowed.
        self.skip_inline_il_layout();
        let type_kw_span = match self.next_non_trivia_raw_at_pos_with_span() {
            Some((Token::Type, span)) => Some(span),
            _ => None,
        };
        if let Some(type_span) = type_kw_span {
            self.drain_raw_up_to(type_span.start);
            self.emit_text(SyntaxKind::TYPE_TOK, type_span);
            self.raw_pos += 1;
            self.skip_inline_il_layout();
            // The `(` must be the *immediate* raw token. Testing `peek()` alone
            // would, for `(# "x" type) (T)` (the closing `)` is swallowed), read
            // the following parenthesised expression as the type argument.
            if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::LParen)) {
                self.bump_into(SyntaxKind::LPAREN_TOK);
                self.parse_type();
                self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `(` after `type` in an inline-IL type argument".to_string(),
                    span,
                });
            }
        }

        // Curried value arguments (`optCurriedArgExprs`) — each an `argExpr`.
        // The loop stops at `:` (return type), `#` (closer), or — crucially —
        // the swallowed outer `)`. When the closing `#` is missing (`(# "x") y`)
        // LexFilter has already removed that `)` from the *filtered* stream, so
        // a bare `peek_starts_app_arg` would leap across it and drag `) y` in as
        // an argument; the raw-stream gate stops at the closer instead. A
        // well-formed inline IL never has `)` directly after an argument (it is
        // always `:` or `#`), so this never blocks a genuine arg.
        loop {
            self.skip_inline_il_layout();
            if self.at_swallowed_inline_il_close() || !self.peek_starts_app_arg() {
                break;
            }
            self.parse_arg_expr();
        }

        // Optional return type (`optInlineAssemblyReturnTypes`): `: typ`, or the
        // unit form `: ()` (FCS's `COLON LPAREN rparen`, an *empty* `retTy`).
        // Same swallowed-`)` guard: never read a `:`/`(` from past the closer.
        // (The arg loop's final `skip_inline_il_layout` already cleared any
        // layout before the `:`.)
        if !self.at_swallowed_inline_il_close()
            && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _)))
        {
            self.bump_into(SyntaxKind::COLON_TOK);
            self.skip_inline_il_layout();
            // After the `:`, the return type (or `()`) must still be inside the
            // inline IL. A swallowed `)` here is a dangling colon (`(# "x" :) ()`),
            // so don't read the following expression as the return type.
            if !self.at_swallowed_inline_il_close() {
                // `: ()` unit return vs `: typ`. The `(` is the immediate raw
                // token (the guard above ruled out a swallowed `)` in between),
                // so `peek()` is that `(` and the lookahead past it is sound.
                let unit_return = match self.peek() {
                    Some((Ok(FilteredToken::Raw(Token::LParen)), span)) => {
                        let end = span.end;
                        matches!(self.next_non_trivia_raw_after(end), Some(Token::RParen))
                    }
                    _ => false,
                };
                if unit_return {
                    self.bump_into(SyntaxKind::LPAREN_TOK);
                    self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
                } else {
                    self.parse_type();
                }
            }
        }

        // The closing `#`, then the swallowed outer `)`. The `#` must precede
        // the `)` on the *raw* stream; if the swallowed `)` is already next
        // (`(# "x")`), the `#` is missing — don't consume a stray `#` from past
        // the closer.
        self.skip_inline_il_layout();
        if !self.at_swallowed_inline_il_close()
            && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Hash)), _)))
        {
            self.bump_into(SyntaxKind::HASH_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `#)` to close the inline-IL expression".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // INLINE_IL_EXPR
        // The closing `)` (LexFilter-swallowed) belongs to the outer PAREN_EXPR.
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        self.builder.finish_node(); // PAREN_EXPR
    }

    /// The IL instruction string of an inline-IL expression — a bare
    /// string-literal token child of `INLINE_IL_EXPR` (not a `CONST_EXPR`;
    /// FCS keeps it as a raw `string`, not a `SynExpr`). FSharp.Core always
    /// writes a regular `"…"`; the verbatim / triple forms are accepted for
    /// robustness. A non-string here is a clean error (the `(#` opener has
    /// already been committed), as is a *byte* string (`"…"B`) — FCS's grammar
    /// consumes the `string` nonterminal, while byte strings lex as `BYTEARRAY`.
    pub(super) fn parse_il_instruction_string(&mut self) {
        // The string must be the immediate raw token. If the swallowed `)` is
        // already next (`(#) "s"`), the string is missing — don't reach past the
        // closer and consume the following string as the instruction.
        let string = if self.at_swallowed_inline_il_close() {
            None
        } else {
            match self.peek() {
                Some((Ok(FilteredToken::Raw(Token::String)), span)) => {
                    Some((SyntaxKind::STRING_LIT, span.clone()))
                }
                Some((Ok(FilteredToken::Raw(Token::VerbatimString)), span)) => {
                    Some((SyntaxKind::VERBATIM_STRING_LIT, span.clone()))
                }
                Some((Ok(FilteredToken::Raw(Token::TripleString)), span)) => {
                    Some((SyntaxKind::TRIPLE_STRING_LIT, span.clone()))
                }
                _ => None,
            }
        };
        match string {
            Some((kind, span)) => {
                // The lexer folds a trailing byte-string `B` into the same
                // `Token::String`/`…`, so detect it from the source slice. FCS
                // rejects a byte-string instruction; flag it but still emit the
                // token (as the string kind) so the tree stays lossless and the
                // instruction is recoverable.
                if self.source[span.clone()].ends_with('B') {
                    self.errors.push(ParseError {
                        message: "a byte string is not a valid inline-IL instruction".to_string(),
                        span,
                    });
                }
                self.bump_into(kind);
            }
            None => {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected an IL instruction string after `(#`".to_string(),
                    span,
                });
            }
        }
    }
}
