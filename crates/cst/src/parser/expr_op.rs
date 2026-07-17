//! Operator expression productions: prefix operators (`parse_minus_expr`,
//! address-of), the infix precedence/continuation lookahead, and the
//! long-ident encoding of operator tokens.

use super::*;

impl<'src> Parser<'src> {
    /// `pars.fsy:5141 minusExpr` — the precedence level where unary
    /// prefix operators bind. Sits between [`Parser::parse_pratt_expr`]
    /// (infix operators) and [`Parser::parse_app_expr`] (function
    /// application + atomic-level prefix operators). The prefix
    /// operators handled here are FCS's `MINUS | PLUS_MINUS_OP "+"/"+."/"-." |
    /// PERCENT_OP "%"/"%%" | AMP | AMP_AMP` (`pars.fsy:5147-5189`).
    ///
    /// Each prefix consumes its operand by recursing back into
    /// `parse_minus_expr` (right-associative stacking — `- - 1` is
    /// `App(~-, App(~-, 1))`), which means the recursive call does NOT
    /// run the Pratt climber. That preserves FCS's precedence ordering:
    /// `- 1 + 2` is `App(+, App(~-, 1), 2)` because the outer
    /// `parse_pratt_expr` picks up the `+` against the already-built
    /// `App(~-, 1)`.
    ///
    /// `MINUS | PLUS_MINUS_OP | PERCENT_OP` produce a plain `APP_EXPR`
    /// over a `LONG_IDENT_EXPR(op_UnaryNegation|op_UnaryPlus|...)` —
    /// matching FCS's `mkSynPrefix`/`mkSynOperator` output. `AMP` /
    /// `AMP_AMP` produce a distinct [`SyntaxKind::ADDRESS_OF_EXPR`] node
    /// instead (FCS's `SynExpr.AddressOf`).
    ///
    /// Sign-folding (`-1` → `Const(Int32 -1)`) is deferred — FCS's
    /// LexFilter does this at the token layer (`LexFilter.fs:2694`);
    /// our LexFilter port doesn't yet, so phase 3.5 produces the
    /// `App(~-, 1)` shape for any unfolded case. The diff tests for
    /// phase 3.5 avoid sign-foldable shapes to keep the FCS oracle
    /// from disagreeing.
    pub(super) fn parse_minus_expr(&mut self) {
        // Depth-guarded: with `parse_atomic_expr`, one of the two universal
        // expression chokepoints. Every `parse_pratt_expr` operand and every
        // minus-level prefix re-enters here — the `-` operand below, and the
        // `&`/`&&` (`parse_address_of`) and `upcast`/`downcast`/`:>`
        // (`parse_inferred_cast`) operands, plus the `if`/`fun`/`match`/`new`
        // dispatch — so a prefix chain (`- - … 0`, `& & … x`, `upcast upcast …`)
        // is bounded here. Guarding the body counts each level; the recursion
        // re-enters this public wrapper.
        self.with_depth(Self::parse_minus_expr_inner);
    }

    fn parse_minus_expr_inner(&mut self) {
        // `if … then … else …` — `Token::If` is in `raw_starts_minus_expr`
        // (so a prefix `-`/`&` accepts it as having an operand), which
        // means the `if`-dispatch must run on every entry to this
        // function, not only the Pratt-level entry. Otherwise the
        // recursive `parse_minus_expr` after a prefix op would fall
        // through to atomic-level `parse_const_expr` and panic on the
        // `If` token. FCS rejects `- if ...` / `& if ...` at the
        // grammar level (`minusExpr`'s operand is `minusExpr`, not
        // `declExpr`), so this path is malformed input — the contract
        // is to produce an error-recovery tree, not a panic.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::If)), _))) {
            self.parse_if_then_else();
            return;
        }
        // `fun <pat>+ -> <body>` — `Virtual::Fun` is LexFilter's rewrite
        // of `Token::Fun`. Like `if`, this dispatch lives here (not at
        // the Pratt entry) so the prefix-operand path also intercepts
        // it rather than falling through to atomic-level and tripping
        // on the virtual. FCS rejects `- fun …` / `& fun …` at the
        // grammar level for the same reason `- if …` is rejected; the
        // diagnostic side would reuse `maybe_warn_keyword_after_prefix`'s
        // pattern in a follow-up if we ever care to emit it (`fun` is a
        // virtual, so it needs a separate arm there).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::Fun)), _))
        ) {
            self.parse_fun_expr();
            return;
        }
        // `do! e` — LexFilter rewrites raw `Token::DoBang` to
        // `Virtual::DoBang` and wraps the body in a SeqBlock
        // (`Virtual::BlockBegin` … `BlockEnd` `DeclEnd`), so it is dispatched
        // here like `fun`/`if` (a virtual at expression-start position),
        // not via the raw-token match below.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::DoBang)), _))
        ) {
            self.parse_do_bang();
            return;
        }
        // `do e` — like `do!`, LexFilter rewrites raw `Token::Do` to
        // `Virtual::Do` and wraps the body in a SeqBlock, so it is dispatched
        // here (a virtual at expression-start position), not via the raw-token
        // match below. At module level this is reached through `parse_module_decl`
        // → `parse_expr` (the `EXPR_DECL` path); in a sequence body it is one
        // `Sequential` element. (The `do` of a `while`/`for` loop never reaches
        // this dispatch — `parse_do_block_body` claims that `Virtual::Do`
        // directly, after the condition's `parse_expr` has already stopped.)
        // FCS rejects `- do …` / `& do …` at the grammar level (a `declExpr`,
        // not a `minusExpr` operand); like `fun`/`do!` above, `do` is a virtual,
        // so the prefix-operand path accepts it leniently (an error-recovery
        // tree, no panic) — the same deferred-diagnostic class as `fun` (a
        // follow-up would extend `maybe_warn_keyword_after_prefix` to the
        // virtuals).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::Do)), _))
        ) {
            self.parse_do_expr();
            return;
        }
        // `let! p = e [in/⏎] body` (and `use!`) — LexFilter rewrites raw
        // `Token::LetBang`/`UseBang` to `Virtual::Binder` and gives the binder
        // the same `CtxtLetDecl` offside scaffolding as plain `let`. Dispatched
        // here like `do!`/`fun` (a virtual at expression-start position).
        //
        // Only the *block-let* form (`Virtual::Binder`, the CE-body `async {
        // let! x = e⏎ … }` shape) is handled. In a *non-block* `CtxtLetDecl`
        // (a parenthesised `(let! x = m in x)`, a `match` guard `when let! …`,
        // an infix RHS `e + let! …`) LexFilter emits a *raw* `Token::LetBang`/
        // `UseBang` instead of the virtual. FCS accepts those (a raw `BINDER …
        // IN …` production); we don't yet — the raw token is kept out of
        // `raw_starts_minus_expr`, so those forms reject cleanly (never panic;
        // pinned by `non_block_bang_binders_reject_without_panicking`).
        // `Virtual::AndBang` only continues an open binder group inside
        // `parse_let_or_use_bang`, never starting a fresh expression.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::Binder)), _))
        ) {
            self.parse_let_or_use_bang();
            return;
        }
        // `let p = e [in/⏎] body` (and `use`) in *expression* position —
        // LexFilter rewrites the raw `Token::Let`/`Use` to `Virtual::Let` with
        // the same `CtxtLetDecl` offside scaffolding as a module-level `let`.
        // Dispatched here like `do!`/binder (a virtual at expression-start
        // position) so the prefix-operand path intercepts it too. A
        // *module-level* `let` reaches `parse_module_let` through
        // `parse_module_decls` (whose `Virtual::Let` arm runs first), so this
        // only fires for an expression-position `let` — a function/`let`/`fun`/
        // `if`/`match` body, a paren body, a tuple element. FCS rejects
        // `- let …` / `& let …` at the grammar level (like `- if …`); this path
        // produces an error-recovery tree rather than panicking, the diagnostic
        // a possible follow-up (`maybe_warn_keyword_after_prefix`).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::Let)), _))
        ) {
            self.parse_let_or_use_expr();
            return;
        }
        // Non-block `let … in` as an expression *operand* — a raw `Token::Let`
        // (LexFilter promotes only a *block-leading* `let` to `Virtual::Let`; a
        // mid-expression one stays raw). Reachable as an infix RHS (`a && let x =
        // e in b`), a tuple element (`1, let …`), or a `lazy`/`assert`/`fixed`
        // operand. Dispatched here (like raw `match`/`try`) so every operand path
        // intercepts it; `parse_let_or_use_expr` handles the raw keyword shape.
        // FCS accepts these as `SynExpr.LetOrUse` (it rejects `- let …` / `& let
        // …` at the prefix-operand grammar level, like `- if …`, but this lenient
        // path recovers rather than panicking).
        //
        // Only plain `let` is dispatched. `use` is excluded on purpose: FCS's
        // inline production relabels a non-block `use … in` binding's leading
        // keyword to `Let` (the block form keeps `Use`), which our text-based
        // `is_use` can't reproduce without misrepresenting the source — so a
        // non-block `use … in` operand stays a rare deferred divergence rather
        // than a wrong-AST one. The bang forms (`let!`/`use!`) are likewise not
        // dispatched (deferred reject-without-panic,
        // `non_block_bang_binders_reject_without_panicking`).
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Let)), _))) {
            self.parse_let_or_use_expr();
            return;
        }
        // `match <scrut> with <clauses>` — `Token::Match` is a raw token
        // (LexFilter does not rewrite it to a virtual; it only pushes
        // `CtxtMatch`). Like `if`/`fun`, the dispatch lives here so the
        // prefix-operand path also intercepts it rather than tripping at
        // atomic level. FCS rejects `- match …` / `& match …` at the grammar
        // level for the same reason `- if …` is rejected, so
        // `maybe_warn_keyword_after_prefix` surfaces the diagnostic at the
        // prefix-operand sites (`Token::Match` arm).
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Match)), _))) {
            self.parse_match_expr();
            return;
        }
        // `match! <scrut> with <clauses>` — `Token::MatchBang` is a raw token
        // (like `Token::Match`, LexFilter pushes `CtxtMatch`/`CtxtMatchClauses`
        // but does not relabel it). Same dispatch placement as `match` so the
        // prefix-operand path intercepts `- match! …` here too;
        // `maybe_warn_keyword_after_prefix` surfaces that diagnostic.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::MatchBang)), _))
        ) {
            self.parse_match_bang_expr();
            return;
        }
        // `while cond do body` — `Token::While` is a raw token (LexFilter pushes
        // `CtxtWhile` but does not relabel it). Like `match`, dispatched here so
        // the prefix-operand path intercepts `- while …` too.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::While)), _))) {
            self.parse_while_expr();
            return;
        }
        // `while! cond do body` — `Token::WhileBang` is a raw token like
        // `Token::While`; same dispatch placement so the prefix-operand path
        // intercepts `- while! …` too.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::WhileBang)), _))
        ) {
            self.parse_while_bang_expr();
            return;
        }
        // `for pat in e do body` — `Token::For` is a raw token (LexFilter pushes
        // `CtxtFor` but does not relabel it). Like `while`, dispatched here so
        // the prefix-operand path intercepts `- for …` too.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::For)), _))) {
            self.parse_for_expr();
            return;
        }
        // `try body with <clauses>` (and, in 10.20b, `try body finally e`) —
        // `Token::Try` is a raw token (LexFilter pushes `CtxtTry` + a one-sided
        // SeqBlock for the body but does not relabel it). Like `match`/`while`/
        // `for`, dispatched here so the prefix-operand path intercepts `- try …`
        // too; FCS rejects `- try …` at the grammar level (`minusExpr`'s
        // operand is `minusExpr`, not `declExpr`), so `maybe_warn_keyword_after_prefix`
        // surfaces that diagnostic at the prefix sites.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Try)), _))) {
            self.parse_try_expr();
            return;
        }
        // `function <clauses>` — `Virtual::Function` is LexFilter's
        // `OFUNCTION` relabel of `Token::Function`. Like `fun`/`match`, the
        // dispatch lives here so the prefix-operand path also intercepts it
        // rather than tripping at atomic level on the virtual.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::Function)), _))
        ) {
            self.parse_function_expr();
            return;
        }
        // `lazy`/`assert` with an offside (later-line) or control-flow operand —
        // LexFilter's `OLAZY`/`OASSERT` relabel of `Token::Lazy`/`Token::Assert`
        // (it also pushed a SeqBlock). Dispatched here beside `Virtual::Function`
        // so the prefix-operand path intercepts it; the same-line raw forms are
        // handled in the raw-token match below. `parse_lazy_or_assert` re-emits
        // the keyword text from the virtual and parses the block operand.
        let lazy_assert_virtual = match self.peek() {
            Some((Ok(FilteredToken::Virtual(Virtual::Lazy)), _)) => {
                Some((SyntaxKind::LAZY_EXPR, SyntaxKind::LAZY_TOK))
            }
            Some((Ok(FilteredToken::Virtual(Virtual::Assert)), _)) => {
                Some((SyntaxKind::ASSERT_EXPR, SyntaxKind::ASSERT_TOK))
            }
            _ => None,
        };
        if let Some((expr_kind, kw_kind)) = lazy_assert_virtual {
            self.parse_lazy_or_assert(expr_kind, kw_kind);
            return;
        }
        // `&` / `&&` → ADDRESS_OF_EXPR; `yield`/`return`/`yield!`/`return!`
        // → YieldOrReturn(From). Like `if`/`fun`, the yield forms are
        // `declExpr`-level keyword prefixes (`pars.fsy:4488`/`:4510`) handled
        // here so the prefix-operand path intercepts them too; FCS accepts a
        // bare `yield 1` at the top level (not only inside a CE), so they are
        // not gated to computation-expression context.
        if let Some((Ok(FilteredToken::Raw(t)), _)) = self.peek().cloned() {
            match t {
                Token::Amp => {
                    self.parse_address_of(SyntaxKind::AMP_TOK);
                    return;
                }
                Token::AmpAmp => {
                    self.parse_address_of(SyntaxKind::AMP_AMP_TOK);
                    return;
                }
                // `^expr` — the from-end index/slice prefix (FCS's `minusExpr:
                // INFIX_AT_HAT_OP minusExpr` with the op exactly `^`,
                // `pars.fsy:5143`, → `SynExpr.IndexFromEnd`). A `minusExpr`-level
                // prefix, so it is valid wherever a `minusExpr` is (an index bound
                // `arr.[^1]`, but also a plain `let i = ^1` or `[ ^1 ]`); the
                // prefix-operand recursion re-enters here. Only a leading `^` (no
                // left operand) is from-end — `a ^ b` is the infix `^` the Pratt
                // loop handles, never reaching this head dispatch.
                Token::Op("^") => {
                    self.parse_from_end_expr();
                    return;
                }
                Token::Yield | Token::Return => {
                    self.parse_yield_or_return(false);
                    return;
                }
                Token::YieldBang | Token::ReturnBang => {
                    self.parse_yield_or_return(true);
                    return;
                }
                // `new T(args)` — object-construction expression. A `minusExpr`
                // production in FCS (`pars.fsy:5173`), so it sits at this level
                // beside the address-of / upcast prefixes; the prefix-operand
                // recursion (`- new T()`) re-enters here and intercepts it too.
                Token::New => {
                    self.parse_new_expr();
                    return;
                }
                // `upcast e` / `downcast e` — the inferred (typeless) coercion
                // prefixes. `minusExpr` productions (`pars.fsy:5182`/`:5185`)
                // → `SynExpr.InferredUpcast`/`InferredDowncast`, sitting beside
                // the address-of / `new` prefixes so the prefix-operand
                // recursion (`- upcast x`) intercepts them too.
                Token::Upcast => {
                    self.parse_inferred_cast(
                        SyntaxKind::INFERRED_UPCAST_EXPR,
                        SyntaxKind::UPCAST_TOK,
                    );
                    return;
                }
                Token::Downcast => {
                    self.parse_inferred_cast(
                        SyntaxKind::INFERRED_DOWNCAST_EXPR,
                        SyntaxKind::DOWNCAST_TOK,
                    );
                    return;
                }
                // `lazy e` / `assert e` — the `declExpr`-level keyword prefixes
                // (`pars.fsy:4346`/`:4349`, → `SynExpr.Lazy`/`SynExpr.Assert`).
                // They sit at FCS's `expr_app` precedence (tighter than every
                // infix operator), so — like `if`/`match`/`new`/`upcast` — the
                // dispatch lives here so the prefix-operand path intercepts them
                // too. Their operand is a `declExpr`, which precedence clips to
                // this `parse_minus_expr` level. `lazy` additionally admits a
                // leading open-lower range (`lazy ..3`); see
                // [`Self::parse_lazy_or_assert`]. FCS rejects
                // `- lazy …` / `& lazy …` (the `minusExpr`-prefix operand is a
                // `minusExpr`, not a `declExpr`), so `lazy`/`assert` are in
                // `maybe_warn_keyword_after_prefix` to surface that diagnostic
                // at the prefix sites.
                Token::Lazy => {
                    self.parse_lazy_or_assert(SyntaxKind::LAZY_EXPR, SyntaxKind::LAZY_TOK);
                    return;
                }
                Token::Assert => {
                    self.parse_lazy_or_assert(SyntaxKind::ASSERT_EXPR, SyntaxKind::ASSERT_TOK);
                    return;
                }
                // `fixed e` — the `declExpr`-level pinning prefix (`pars.fsy:4624
                // FIXED declExpr`, → `SynExpr.Fixed`). Dispatched here beside
                // `lazy`/`assert`/`if`/`new` so the prefix-operand path
                // intercepts it too. But unlike `lazy`/`assert` (which clip their
                // operand tight via `%prec expr_lazy`), `FIXED declExpr` has *no*
                // `%prec`, so its operand binds looser than every infix operator
                // — see [`Self::parse_fixed`]. FCS rejects `- fixed …` / `&
                // fixed …`, so `fixed` is in `maybe_warn_keyword_after_prefix`.
                Token::Fixed => {
                    self.parse_fixed();
                    return;
                }
                _ => {}
            }
        }
        // MINUS | PLUS_MINUS_OP | PERCENT_OP — emit `APP_EXPR > [op-as-long-ident, operand]`
        // matching FCS's `mkSynPrefix`/`mkSynOperator` shape. Only the
        // exact eligible operator-text set listed in
        // [`Parser::op_is_minus_expr_prefix`] qualifies — `-_op`, `-/`,
        // etc. are explicitly excluded by FCS's grammar carve-out.
        if self.op_is_minus_expr_prefix() {
            let cp = self.builder.checkpoint();
            self.emit_prefix_op_as_long_ident();
            self.maybe_warn_keyword_after_prefix();
            // The operand is a `minusExpr`, which excludes the open-lower range
            // `..e` (a `declExpr` only) — FCS rejects `- ..3`. Guarding on
            // `!peek_is_range_op` keeps a leading `..` out of the recursion, so
            // it lands as a clean missing-operand error instead of reaching the
            // atomic const parser's `unreachable!`. The bare `*` wildcard is an
            // atom (`parse_index_wildcard`), so it flows through as an operand
            // (`- *` → `App(~-, IndexRange(None,None))`); FCS offside-rejects the
            // top-level form, a lenient divergence (see the plan).
            if self.peek_is_expr_start() && !self.peek_is_range_op() {
                // The recursive operand is counted by this function's own body
                // guard (it re-enters the public `parse_minus_expr`).
                self.parse_minus_expr();
            } else {
                self.push_missing_operand_error();
            }
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::APP_EXPR));
            self.builder.finish_node();
            return;
        }
        // The whole-dimension wildcard `*` (FCS's nullary `STAR`, phase 10.22a).
        // FCS reduces it as a `declExpr` leaf, **not** an `atomicExpr`: it can be
        // an infix operand (`* + 1`, `1 + *`, handled by the Pratt climber whose
        // operand is this `minusExpr` level) and a range bound (`* .. 3`), but it
        // is **not** applicable or dottable — `* x` / `*.Length` / `* (n)` are
        // FCS parse errors. Emitting the leaf *here* (above `parse_app_expr` and
        // the postfix tail, returning immediately) gives the infix / range
        // behaviour while stopping the application / postfix loops from attaching
        // a following atom. Exactly the lone `Op("*")`; a glued `**` / `*..` is a
        // different op token and falls through to the application level.
        if self.peek_is_index_wildcard_star() {
            self.parse_index_wildcard();
            return;
        }
        self.parse_app_expr();
    }

    /// Wrap the current op-token + a recursively-parsed operand under
    /// an [`SyntaxKind::ADDRESS_OF_EXPR`]. FCS's `mkSynPrefixPrim`
    /// (`SyntaxTreeOps.fs:483-486`) special-cases `~&` / `~&&` to
    /// produce `SynExpr.AddressOf` rather than the `App` shape used
    /// for other prefixes. The operand is parsed at the same
    /// precedence level (`parse_minus_expr` again) so chained forms
    /// like `&&x` over `&(...)`, or `& - 1`, lower the right way.
    pub(super) fn parse_address_of(&mut self, op_kind: SyntaxKind) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ADDRESS_OF_EXPR));
        self.bump_into(op_kind);
        self.maybe_warn_keyword_after_prefix();
        // As with the `-`/`+`/`%` prefixes, the `&`/`&&` operand is a
        // `minusExpr`, so the open-lower range `..e` is excluded (FCS rejects
        // `& ..3`) — guard it out of the recursion to keep the leading `..` away
        // from the atomic const parser's `unreachable!`. The `*` wildcard is an
        // atom and flows through (lenient on the FCS-offside-rejected `& *`).
        if self.peek_is_expr_start() && !self.peek_is_range_op() {
            self.parse_minus_expr();
        } else {
            self.push_missing_operand_error();
        }
        self.builder.finish_node();
    }

    /// `^expr` — the from-end index/slice prefix (FCS's `minusExpr:
    /// INFIX_AT_HAT_OP minusExpr` with the operator exactly `^`) →
    /// [`SyntaxKind::INDEX_FROM_END_EXPR`]. The `^` is emitted as a
    /// [`SyntaxKind::HAT_TOK`]; the operand is a `minusExpr` (so `^3` is
    /// `IndexFromEnd 3`, `^a.b` is `IndexFromEnd a.b`). Like the `&`/`&&` prefix,
    /// the open-lower range `..e` is excluded from the operand (keeping a leading
    /// `..` away from the atomic const parser's `unreachable!`); a bare `^` with
    /// no operand records a clean missing-operand error. Caller has verified the
    /// cursor is at `Token::Op("^")`.
    pub(super) fn parse_from_end_expr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INDEX_FROM_END_EXPR));
        self.bump_into(SyntaxKind::HAT_TOK);
        // Like the other `minusExpr` prefixes, a `declExpr`-level keyword operand
        // (`^ if …`, `^ match …`) is an FCS error (FS0010); surface the same
        // diagnostic before recursing.
        self.maybe_warn_keyword_after_prefix();
        if self.peek_is_expr_start() && !self.peek_is_range_op() {
            self.parse_minus_expr();
        } else {
            self.push_missing_operand_error();
        }
        self.builder.finish_node();
    }

    /// `pars.fsy:5182 UPCAST minusExpr` / `pars.fsy:5185 DOWNCAST minusExpr`
    /// — the inferred (typeless) coercion prefixes, FCS's
    /// `SynExpr.InferredUpcast(expr, range)` / `InferredDowncast(expr, range)`.
    /// Emits `<expr_kind> > [<kw_kind>, <inner-expr>]`. The caller
    /// ([`Parser::parse_minus_expr`]) has verified the cursor is at the matching
    /// `upcast` / `downcast` keyword.
    ///
    /// Structurally the same prefix shape as [`Parser::parse_address_of`]: the
    /// operand is parsed at the same precedence level (`parse_minus_expr`
    /// again, FCS's `minusExpr` operand), so a chained `upcast downcast x`
    /// nests right and a sign prefix (`upcast -1`) lowers under the cast. A
    /// `declExpr`-level keyword operand (`upcast if …`) is rejected by
    /// [`Parser::maybe_warn_keyword_after_prefix`] (FCS's operand is
    /// `minusExpr`, not `declExpr`); a missing operand records the
    /// missing-operand error and closes the node losslessly.
    pub(super) fn parse_inferred_cast(&mut self, expr_kind: SyntaxKind, kw_kind: SyntaxKind) {
        self.builder.start_node(FSharpLang::kind_to_raw(expr_kind));
        self.bump_into(kw_kind);
        self.maybe_warn_keyword_after_prefix();
        // Like the `-`/`&`/`%` prefixes, the `upcast`/`downcast` operand is a
        // `minusExpr`, so the open-lower range `..e` is excluded — guard it out
        // of the recursion so `upcast ..3` lands as a clean missing-operand error
        // rather than feeding the leaf to the atomic const parser's
        // `unreachable!`. The `*` wildcard is an atom and flows through (lenient
        // on the FCS-offside-rejected `upcast *`).
        if self.peek_is_expr_start() && !self.peek_is_range_op() {
            self.parse_minus_expr();
        } else {
            self.push_missing_operand_error();
        }
        self.builder.finish_node();
    }

    /// `pars.fsy:4346 LAZY declExpr %prec expr_lazy` / `pars.fsy:4349 ASSERT
    /// declExpr %prec expr_assert` — the delayed-computation / runtime-assertion
    /// keyword prefixes, FCS's `SynExpr.Lazy(expr, range)` /
    /// `SynExpr.Assert(expr, range)`. Emits `<expr_kind> > [<kw_kind>,
    /// <inner-expr>]`. The caller ([`Parser::parse_minus_expr`]) has verified the
    /// cursor is at the matching `lazy` / `assert` keyword.
    ///
    /// Both keywords sit at FCS's `expr_app` precedence (tighter than every
    /// infix operator) yet take a grammatical `declExpr` operand. Precedence
    /// clips that operand to exactly this codebase's `minusExpr` level — the
    /// recursive `parse_minus_expr` call. `lazy` also admits a leading
    /// open-lower range, the one `declExpr` form `minusExpr` lacks
    /// (`lazy ..3` = `Lazy(IndexRange(None, 3))`; FCS accepts it, unlike
    /// `assert ..3` / `upcast ..3`). So:
    ///
    ///  * `lazy f y` = `Lazy(App(f, y))`, `lazy -y` = `Lazy(-y)`, `lazy a.b` =
    ///    `Lazy(a.b)`, `lazy if … ` = `Lazy(IfThenElse …)` — the operand absorbs
    ///    application, postfix, unary minus, and the control-flow keywords (all
    ///    reached through `parse_minus_expr`);
    ///  * `lazy a + b` = `(lazy a) + b`, `lazy a :: b` = `(lazy a) :: b`,
    ///    `lazy a, b` = `(lazy a), b`, `lazy a .. b` = `(lazy a) .. b`,
    ///    `lazy a :> T` = `(lazy a) :> T`, `lazy a := b` = `(lazy a) := b` —
    ///    the infix / cons / tuple / left-bounded-range / type-relation / `:=`
    ///    continuations are picked up by the enclosing loops against the whole
    ///    `LAZY_EXPR`;
    ///  * `lazy a <- b` = `Lazy(Set(a, b))` — the **one** binary continuation
    ///    that folds *into* the operand. FCS's `declExpr` includes `minusExpr
    ///    LARROW declExprBlock`, and the `declExpr: minusExpr` reduction has no
    ///    precedence, so yacc shifts the `<-` into the operand (verified against
    ///    FCS; the looser `:=` / type-relation ops do *not* fold in).
    ///
    /// That operand shape — a `minusExpr` plus an optional trailing `<-`, but no
    /// looser binary continuation — is exactly what [`Self::parse_pratt_expr`]
    /// produces with `min_bp = u16::MAX`: every infix / cons / type-relation
    /// continuation is `lbp`-gated and so suppressed, while the `<-` tail is
    /// checked *unconditionally* (it binds a `minusExpr` LHS) and still fires.
    /// Reusing that frame also inherits its `<-` swallowed-closer / wildcard
    /// gates rather than duplicating them. The lazy-only leading open-lower
    /// range is the one `declExpr` form `parse_pratt_expr` cannot reach (it
    /// descends through `parse_minus_expr`, which excludes a bare `..`), so it is
    /// dispatched directly here.
    ///
    /// Unlike `upcast`/`downcast` ([`Self::parse_inferred_cast`]), the operand
    /// is **not** run through [`Self::maybe_warn_keyword_after_prefix`]: a
    /// `declExpr` operand legitimately *is* a control-flow keyword (`lazy if …`
    /// is valid F#), so warning there would reject input FCS accepts. A missing
    /// operand records the shared missing-operand error and closes the node
    /// losslessly (FCS errors too — for `assert` with its dedicated "not a
    /// first-class value" diagnostic, for `lazy` with a plain syntax error).
    pub(super) fn parse_lazy_or_assert(&mut self, expr_kind: SyntaxKind, kw_kind: SyntaxKind) {
        self.builder.start_node(FSharpLang::kind_to_raw(expr_kind));

        // Emit the keyword. The same-line form arrives as the raw `Token::Lazy`/
        // `Token::Assert` (a plain `bump_into`); the offside form arrives as the
        // `Virtual::Lazy`/`Virtual::Assert` relabel (LexFilter's `OLAZY`/`OASSERT`
        // — a `CtxtSeqBlock` was pushed with it), whose raw keyword still sits at
        // `raw_pos` with the virtual's span, so re-emit the text via the shared
        // drain + `emit_text` idiom (as `parse_do_expr` does for `Virtual::Do`).
        let offside = matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::Lazy | Virtual::Assert)),
                _
            )),
        );
        if offside {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .expect("parse_lazy_or_assert offside path without a peeked virtual");
            self.drain_raw_up_to(span.start);
            debug_assert!(
                matches!(
                    self.raw_tokens.get(self.raw_pos),
                    Some((Ok(TriviaToken::Lexed(Token::Lazy | Token::Assert)), s)) if *s == span,
                ),
                "Virtual::Lazy/Assert must be backed by a raw lazy/assert at raw_pos with matching span"
            );
            self.emit_text(kw_kind, span);
            self.raw_pos += 1;
            self.pos += 1;
        } else {
            self.bump_into(kw_kind);
        }

        // Offside-block operand. The lex-filter pushed a `CtxtSeqBlock` after the
        // relabelled keyword (`pushes.rs`, `LexFilter.fs:2232`), so the operand
        // leads with a `Virtual::BlockBegin`. Parse the whole offside block as the
        // operand — a full `declExpr` that absorbs infix continuations and
        // sequences the statements (`lazy⏎ a⏎ |> b` = `Lazy(a |> b)`, `lazy⏎ f a⏎
        // g b` = `Lazy(Sequential(f a, g b))`) — rather than the tight
        // `parse_pratt_expr` clip the single-line form below uses. `parse_if_body`
        // gathers the multi-statement body and consumes the matching `BlockEnd`.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)),
        ) {
            let branch = if kw_kind == SyntaxKind::LAZY_TOK {
                "lazy"
            } else {
                "assert"
            };
            self.bump_into(SyntaxKind::ERROR);
            self.parse_if_body(branch, true);
            self.builder.finish_node();
            return;
        }

        if self.at_swallowed_seq_closer() {
            // `lazy`/`assert` is the last token inside a paren/brace, so
            // LexFilter has stripped the closing `)`/`}` from the filtered
            // stream and `peek` surfaces the token *past* it (`(lazy) y`). The
            // keyword has no real operand — the closer belongs to the enclosing
            // construct — so record a missing operand rather than dragging the
            // outer token across the swallowed closer. The same discipline the
            // range / Pratt continuations apply (see
            // [`Self::at_swallowed_seq_closer`]). Recovery only: a valid `lazy e`
            // always has its operand before any closer, so this never fires on
            // well-formed input.
            self.push_missing_operand_error();
        } else if self.peek_is_range_op() && kw_kind == SyntaxKind::LAZY_TOK {
            // The lazy-only leading open-lower range `..e` — a `declExpr` the
            // `parse_pratt_expr` path below would reject (it descends to
            // `parse_minus_expr`, whose `!peek_is_range_op` guard keeps a bare
            // `..` out of the atomic const parser's `unreachable!`). FCS's
            // `LAZY declExpr` operand admits it, so parse it directly; `ASSERT
            // ..3` is rejected by FCS and falls through to the missing-operand
            // recovery below.
            //
            // `parse_open_lower_range` parses the upper at the range level, so a
            // chained open-lower operand (`lazy ..a .. b`) stays under this node
            // as `Lazy(IndexRange(None, IndexRange(a, b)))`. A left-bounded range
            // after the operand (`lazy a .. b`) still belongs to the enclosing
            // range loop and is `(lazy a) .. b`.
            self.parse_open_lower_range();
        } else if self.peek_is_range_op() {
            self.push_missing_operand_error();
        } else if self.peek_is_expr_start() {
            // `u16::MAX` suppresses every binary continuation (all have
            // `lbp < u16::MAX`) while leaving the unconditional `<-` tail live —
            // the `minusExpr` + optional `<-` operand grammar (see above).
            self.parse_pratt_expr(u16::MAX);
        } else {
            self.push_missing_operand_error();
        }
        self.builder.finish_node();
    }

    /// `pars.fsy:4624 FIXED declExpr` — the `fixed e` pinning prefix, FCS's
    /// `SynExpr.Fixed(expr, range)`. Emits `FIXED_EXPR > [FIXED_TOK,
    /// <inner-expr>]`. The caller ([`Parser::parse_minus_expr`]) has verified the
    /// cursor is at the `fixed` keyword.
    ///
    /// `fixed` *looks* like [`Self::parse_lazy_or_assert`] — a keyword prefix
    /// over a `declExpr` operand — but binds the **opposite** way. `LAZY/ASSERT
    /// declExpr %prec expr_lazy/expr_assert` clip their operand tight (FCS's
    /// `expr_app` precedence, so `lazy a + b` = `(lazy a) + b`). `FIXED declExpr`
    /// carries *no* `%prec`, so the rule inherits its rightmost terminal's
    /// precedence (`FIXED`, which has none) and every shift/reduce conflict
    /// defaults to *shift*: the operand greedily absorbs the whole `declExpr`.
    /// Verified against FCS — `fixed a + b` = `Fixed(a + b)`, `fixed a, b` =
    /// `Fixed(Tuple(a, b))`, `fixed a := b`, `fixed a <- b`, `fixed a :> T` =
    /// `Fixed(Upcast(a, T))`, `fixed if … `, `fixed fun … `, `fixed ..3` all fold
    /// *into* the operand. Only `: T` (Typed, at `typedSequentialExpr`), `;`
    /// (Sequential), and `in` stay outside: `fixed a : T` = `Typed(Fixed a, T)`.
    /// (A *bare* inline `fixed let … in …` is the one form FCS folds in that we
    /// don't — a non-block `let … in` surfaces as a raw `Token::Let` the operand
    /// parser doesn't handle; shared with `lazy`/`assert`, see `fcs-divergences.md`.
    /// The parenthesised `fixed (let … in …)` works.)
    ///
    /// A full `declExpr` is exactly what [`Self::parse_expr`] parses (tuple `,`,
    /// `:=`, range, infix, cons, type-relation, `<-`, and — via
    /// `parse_minus_expr` — the control-flow keywords and a *leading* open-lower
    /// range). So the operand is just `parse_expr` — no Pratt-clipping, and no
    /// special open-lower-range arm (unlike `lazy`/`assert`, whose tight
    /// `parse_pratt_expr` frame cannot reach a leading `..`; here `parse_expr` →
    /// `parse_range_expr` handles it).
    ///
    /// FCS rejects `f fixed x` (a `declExpr` is not an `atomicExpr` application
    /// arg) and `- fixed …` / `& fixed …` (the prefix operand is a `minusExpr`);
    /// the latter is surfaced via [`Self::maybe_warn_keyword_after_prefix`] at the
    /// prefix sites. A missing operand records the shared missing-operand error
    /// and closes the node losslessly (FCS errors too). The swallowed-closer gate
    /// matches [`Self::parse_lazy_or_assert`]: a `fixed` as the last token inside
    /// `( … )` has its `)` stripped by LexFilter, so the operand must not be
    /// dragged across it (`(fixed) y`).
    pub(super) fn parse_fixed(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::FIXED_EXPR));
        self.bump_into(SyntaxKind::FIXED_TOK);
        if self.at_swallowed_seq_closer() {
            // `fixed` is the last token inside a paren/brace, so LexFilter has
            // stripped the closing `)`/`}` and `peek` surfaces the token *past*
            // it (`(fixed) y`). The keyword has no real operand — the closer
            // belongs to the enclosing construct — so record a missing operand
            // rather than parsing across the swallowed closer. Recovery only: a
            // valid `fixed e` always has its operand before any closer.
            self.push_missing_operand_error();
        } else if self.peek_is_expr_start() || self.peek_is_range_op() {
            // The full `declExpr` operand. `peek_is_range_op` admits a *leading*
            // open-lower range (`fixed ..3`); `parse_expr` → `parse_range_expr`
            // builds it (the swallowed-closer case is already excluded above, so
            // `at_range_op` inside fires).
            self.parse_expr();
        } else {
            self.push_missing_operand_error();
        }
        self.builder.finish_node();
    }

    /// `pars.fsy:5173 NEW atomType opt_HIGH_PRECEDENCE_APP atomicExprAfterType`
    /// — an object-construction expression `new T(args)`, FCS's
    /// `SynExpr.New(isProtected = false, targetType, expr, range)`. Emits
    /// [`SyntaxKind::NEW_EXPR`]; the caller ([`Parser::parse_minus_expr`]) has
    /// verified the cursor is at the raw `new` keyword.
    ///
    /// Structurally identical to the base-construction half of
    /// [`Parser::parse_inherit_member`] (`inherit Base(args)`): the target type
    /// is FCS's `atomType` ([`Parser::parse_atomic_type`], so `Foo<int>` keeps
    /// its `<…>` and a following `(` opens the args, and `System.Object`'s dotted
    /// path stays in the type), and the argument is `atomicExprAfterType` behind
    /// an optional `HighPrecedenceParenApp` adjacency marker (consumed zero-width
    /// as `ERROR`, FCS's elided `opt_HIGH_PRECEDENCE_APP`).
    ///
    /// The argument is parsed **head-only** ([`Parser::parse_atomic_expr_head`]),
    /// matching `atomicExprAfterType` (a self-contained atom — constant, paren,
    /// brace, …, with no postfix tail of its own). So `new T().Member` does not
    /// fold the `.Member` onto the unit argument; FCS makes that a *separate*
    /// error production (`parsNewExprMemberAccess`, requiring `(new T()).Member`),
    /// and leaving the `.Member` for the enclosing context matches that.
    ///
    /// A missing argument is FCS's `NEW atomType opt_HIGH_PRECEDENCE_APP error`
    /// recovery (`SynExpr.New(_, type, ArbitraryAfterError, _)` plus a parse
    /// error): we record the error and close the `NEW_EXPR` carrying just the
    /// type, staying lossless.
    pub(super) fn parse_new_expr(&mut self) {
        // The `new` keyword's own span, captured *before* the bump. The
        // object-expression brace handler ([`Parser::parse_obj_or_computation_brace`])
        // stamps `obj_brace_base_new` with the span of the brace's head `new`, so
        // comparing against it tells whether *this* `new` is that head — the only
        // `new` allowed to become the bare `{ new T }` object expression below.
        let new_span = self.peek().map(|(_, s)| s.clone());
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::NEW_EXPR));
        self.bump_into(SyntaxKind::NEW_TOK);
        // The constructed type — FCS's `atomType`.
        if self.peek_starts_atomic_type() {
            self.parse_atomic_type();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a type after `new`".to_string(),
                span,
            });
        }
        // Skip the `HighPrecedenceParenApp` virtual of an adjacent `(` (a space
        // before the `(` — the `new Foo ()` form — means there is no marker).
        // Consumed zero-width as `ERROR`, the idiom `parse_inherit_member` /
        // the implicit-ctor parser use for FCS's elided `opt_HIGH_PRECEDENCE_APP`.
        if self.peek_is_paren_app_marker() {
            self.bump_into(SyntaxKind::ERROR);
        }
        // The constructor argument (`atomicExprAfterType`). Shared gate
        // ([`Self::peek_starts_aftertype_arg`]) with `parse_inherit_member` /
        // `parse_attribute`: a `(` uses the `parse_atomic_expr` LParen
        // precondition (minus the `( op )` operator-value, excluded from
        // `atomicExprAfterType`), everything else the `atomicExprAfterType`
        // starters (which exclude a bare ident and the prefix-operator forms).
        // A missing argument is the error-recovery path.
        let starts_arg = self.peek_starts_aftertype_arg();
        if starts_arg {
            self.parse_atomic_expr_head();
        } else if new_span.is_some()
            && new_span == self.obj_brace_base_new
            && matches!(self.next_non_trivia_raw_at_pos(), Some(Token::RBrace))
        {
            // No args and the brace closes directly after the type: the bare
            // `{ new T }` object expression (FCS's `objExpr` alt `NEW atomType`),
            // where a missing constructor argument is legal (`argOptions = None`).
            // Gated on *both* this `new` being the brace's head (`obj_brace_base_new`
            // — so a nested or trailing argless `new`, which FCS rejects, never
            // qualifies) *and* the next significant raw token being the swallowed
            // `}` (so `{ new T :> IFoo }`, which FCS errors on, still takes the
            // error path below rather than masquerading as the bare form). Flag it
            // for the handler, which rewraps this `NEW_EXPR` as the `OBJ_EXPR`
            // carrier; suppress the arg error.
            //
            // Checked **before** the `with`/`interface` branch: those test the
            // *filtered* `peek()`, which skips LexFilter-swallowed closers, so for
            // a bare object expression nested directly before an enclosing brace's
            // `with`/`interface` — the inner `{ new Bar }` in `{ new Foo({ new Bar
            // }) with member … }` — `peek()` would surface the *outer* `with` and
            // wrongly suppress this as the with-form base. The `next-raw == }`
            // test reads the *raw* stream (the inner brace's own swallowed `}` is
            // the next significant raw token), so it correctly recognises the inner
            // bare form first. The handler's with-emission carries the symmetric
            // raw guard so it likewise leaves the outer `with` to the outer brace.
            self.obj_brace_base_no_arg = true;
        } else if new_span.is_some()
            && new_span == self.obj_brace_base_new
            && (matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::With)), _)))
                || matches!(
                    self.peek(),
                    Some((
                        Ok(FilteredToken::Virtual(
                            Virtual::With | Virtual::InterfaceMember
                        )),
                        _
                    ))
                ))
        {
            // No args, but a `with` or `interface` follows: this `new T` is the
            // base call of an *object expression* (`{ new T with member … }`, the
            // value-binding `{ new T with X = e }`, or the interface-only `{ new T
            // interface I with … }`, FCS's `objExpr` alt `NEW atomType` feeding
            // `objExprBindings` / `objExprInterfaces`), where a missing argument is
            // legal (FCS `argOptions = None`). The member-form `with` is a raw
            // `Token::With`; the value-binding `with` is the `OWITH` relabel
            // (`Virtual::With`); the interface is the `OINTERFACE_MEMBER` relabel
            // (`Virtual::InterfaceMember`). The brace handler
            // ([`Parser::parse_obj_or_computation_brace`]) rewraps this `NEW_EXPR`
            // as the carrier of an `OBJ_EXPR`; suppress the arg error.
            //
            // Gated on this `new` being the brace's head (`new_span ==
            // obj_brace_base_new`), like the bare branch above: the filtered
            // `peek()` skips LexFilter-swallowed closers, so a *nested* argless
            // `new` in a constructor argument — the inner `new Bar` in `{ new
            // Foo(new Bar) with X = 1 }` — would see the *outer* `with`/`interface`
            // past the swallowed `)` and wrongly suppress its missing-argument
            // error, which FCS reports. The span guard restricts suppression to the
            // actual base call, so the nested `new` keeps its error.
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected constructor arguments after the type in a `new` expression"
                    .to_string(),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// FCS rejects `- if …` / `& if …` / `- match …` / `& match …` at the
    /// grammar level: `minusExpr`'s operand is `minusExpr`, not `declExpr`,
    /// and the FCS parser raises a "syntax error" against the `IF` / `MATCH`
    /// token. Our `Token::If` and `Token::Match` both sit in
    /// `raw_starts_minus_expr` so [`Parser::parse_minus_expr`]'s `if`- and
    /// `match`-dispatch can intercept them (otherwise the recursive
    /// minus-operand call falls through to atomic-level and panics on the
    /// keyword). That keeps the tree shape sensible but would otherwise
    /// *silently accept* malformed input — call this from each
    /// prefix-operand parse site to surface the diagnostic.
    pub(super) fn maybe_warn_keyword_after_prefix(&mut self) {
        let Some((Ok(ft), span)) = self.peek().cloned() else {
            return;
        };
        let keyword = match ft {
            FilteredToken::Raw(Token::If) => "if",
            FilteredToken::Raw(Token::Match) => "match",
            FilteredToken::Raw(Token::MatchBang) => "match!",
            FilteredToken::Raw(Token::While) => "while",
            FilteredToken::Raw(Token::WhileBang) => "while!",
            FilteredToken::Raw(Token::For) => "for",
            FilteredToken::Raw(Token::Try) => "try",
            // `lazy`/`assert` are `declExpr`-level prefixes (not `minusExpr`), so
            // FCS rejects `- lazy …` / `& lazy …` / `upcast lazy …` exactly as it
            // rejects `- if …`. Surface the same diagnostic at the prefix sites.
            // Both spellings reach here: the same-line raw `Token::Lazy`/`Assert`
            // *and* the offside/control-flow `OLAZY`/`OASSERT` relabel
            // (`- lazy⏎ …` / `& lazy if …`), so match the virtual too — else the
            // relabelled form would slip through with no diagnostic.
            FilteredToken::Raw(Token::Lazy) | FilteredToken::Virtual(Virtual::Lazy) => "lazy",
            FilteredToken::Raw(Token::Assert) | FilteredToken::Virtual(Virtual::Assert) => "assert",
            // `fixed` is a `declExpr`-level prefix (not `minusExpr`), so FCS
            // rejects `- fixed …` / `& fixed …` exactly as it rejects `- if …`.
            FilteredToken::Raw(Token::Fixed) => "fixed",
            _ => return,
        };
        self.errors.push(ParseError {
            message: format!(
                "`{keyword}` cannot appear directly after a prefix operator; \
                 wrap the expression in parentheses"
            ),
            span,
        });
    }

    /// `true` if the current filtered token is an operator-text the
    /// minusExpr rule consumes as a prefix. Mirrors `pars.fsy:5147-5167`'s
    /// list: `MINUS` (lexed as `Op("-")` here), `PLUS_MINUS_OP "+"` /
    /// `"+."` / `"-."` / `"?+"` / `"?-"`, `PERCENT_OP "%"` / `"%%"`.
    /// `?+`/`?-` match FCS's `IsValidPrefixOperatorUse`
    /// (`PrettyNaming.fs:624-639`) — they're the only `?`-prefixed
    /// PLUS_MINUS_OP variants FCS classifies as valid prefix ops; other
    /// shapes like `??+` parse via the same grammar rule but emit an
    /// "invalid prefix operator" diagnostic in FCS, which we don't model
    /// at the parser layer. Bare `Token::Amp` / `Token::AmpAmp` are
    /// handled separately (they take the `ADDRESS_OF_EXPR` branch).
    pub(super) fn op_is_minus_expr_prefix(&self) -> bool {
        let Some((Ok(FilteredToken::Raw(Token::Op(text))), _)) = self.peek() else {
            return false;
        };
        matches!(*text, "-" | "+" | "+." | "-." | "?+" | "?-" | "%" | "%%")
    }

    /// Emit the current filtered op token as `LONG_IDENT_EXPR >
    /// LONG_IDENT > IDENT_TOK("<src>")` — the same encoding used by the
    /// infix path (see [`Parser::emit_infix_op_as_long_ident`]). FCS
    /// stamps `IdentTrivia.OriginalNotation` with the source text and
    /// mangles `Ident.idText` (`op_UnaryNegation`, `op_BangPlus`, …);
    /// our green-tree token *is* the source text, which the FCS-side
    /// normaliser already unwraps via `OriginalNotation` so the diff
    /// lines up.
    pub(super) fn emit_prefix_op_as_long_ident(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_EXPR));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        self.bump_into(SyntaxKind::IDENT_TOK);
        self.builder.finish_node();
        self.builder.finish_node();
    }

    /// `Some((lbp, rbp))` if the next filtered token is an infix
    /// operator the Pratt climber should pick up, `None` otherwise.
    /// Two gates beyond the bare token classification:
    ///
    /// * **LexFilter-swallowed `RParen` ahead.** Same rule as
    ///   [`Parser::at_tuple_continuation`] / [`Parser::at_app_continuation`]:
    ///   a recursive `parse_expr` inside `( inner )` must not pull a
    ///   post-`)` operator into the inner expression. Without this gate
    ///   `(a) + b` would parse as `Paren(App(+, a, b))`.
    ///
    /// * **RHS atom-starter required.** A token that *looks* like an
    ///   infix op can also appear at the head of a construct Phase 3.4
    ///   doesn't model — `=` in `let x = 1` is the assignment of a
    ///   let-binding, followed by `Virtual::BlockBegin` rather than an
    ///   expression atom. Treating it as infix would dive into
    ///   `parse_atomic_expr` on the virtual and hit
    ///   `parse_const_expr`'s `unreachable!`. We bail here so the outer
    ///   impl_file loop can absorb the `=` (and the rest of the
    ///   binding) as recovery errors, keeping the tree lossless.
    pub(super) fn peek_infix_continuation(&self) -> Option<(u16, u16)> {
        let bp = self.peek_infix_op()?;
        // Swallowed-closer gate. LexFilter strips a paren expression's `)`
        // and a brace expression's `}` from the filtered stream
        // (`parse_paren_expr` / `parse_brace_expr` recover them from
        // the raw stream), so an infix operator that sits *after* the closer
        // in source surfaces as the immediate filtered successor of the body.
        // If the next non-trivia raw token is that swallowed closer, the
        // operator belongs to the enclosing expression — bail so the body
        // stops at the closer and the outer Pratt loop takes the operator
        // (`( 1 ) + 2`, `{ 1 } + 2`).
        for (res, _) in self.raw_tokens.iter().skip(self.raw_pos) {
            match res {
                Ok(t) if raw_is_trivia(t) => continue,
                Ok(TriviaToken::Lexed(Token::RParen | Token::RBrace)) => return None,
                _ => break,
            }
        }
        // Swallowed-closer-*after* gate (mirrors `peek_cons_continuation`): the
        // operator's RHS may be a LexFilter-swallowed `)` / `}` (`(1 +) x`,
        // `{1 +} y`). The RHS-must-exist gate below is a *filtered* lookahead, so
        // it peers past the stripped closer and would wrongly see the enclosing
        // token as the RHS — building the infix `App` across the closer. A raw
        // lookahead past the operator's span catches it; leave the operator for
        // enclosing recovery.
        if let Some((_, op_span)) = self.peek()
            && self.op_rhs_is_swallowed_closer(op_span.end)
        {
            return None;
        }
        // RHS-must-exist gate.
        if !self.is_expr_start_at(self.pos + 1) {
            return None;
        }
        // ADJACENT_PREFIX_OP gate.
        if self.op_is_adjacent_prefix() {
            return None;
        }
        Some(bp)
    }

    /// `Some((node_kind, op_tok_kind, lbp))` if the next filtered token is one
    /// of the three type-relation operators the Pratt climber wraps into a
    /// dedicated node (rather than the `mkSynInfix` two-tier `App`):
    ///
    /// * `:?`  (`Token::ColonQMark`)        → `TYPE_TEST_EXPR` (`SynExpr.TypeTest`)
    /// * `:>`  (`Token::ColonGreater`)      → `UPCAST_EXPR`    (`SynExpr.Upcast`)
    /// * `:?>` (`Token::ColonQMarkGreater`) → `DOWNCAST_EXPR`  (`SynExpr.Downcast`)
    ///
    /// Each consumes a *type* on the right (FCS's `typ`), not an expression, so
    /// the caller follows the op token with [`Parser::parse_type`]. All three are
    /// left-associative (`pars.fsy:358`/`:363`). Precedence: `:>` / `:?>` sit just
    /// below the compare bucket (lbp `25`, against the compare ops' rbp `31`); `:?`
    /// sits between `::`/`@^` and `+`/`-` (lbp `55`, above the compare rbp `31` and
    /// the `@^` band `40`, below `+`/`-`'s rbp `61`). The `rbp` is irrelevant —
    /// the RHS is a self-delimiting type, never re-entered through the Pratt
    /// climber — so only the `lbp` (governing whether a tighter left operator
    /// absorbs the cast) is returned.
    ///
    /// Applies the same swallowed-`)`/`}` gate as
    /// [`Parser::peek_infix_continuation`]: inside `( e )` / `{ e }` the closer is
    /// stripped from the filtered stream, so a type-op *after* the closer surfaces
    /// as the body's immediate filtered successor. Without the gate `(a) :?> b`
    /// would build the cast *inside* the paren and then fail to find the swallowed
    /// `)`; bail so the body stops at the closer and the enclosing frame takes the
    /// operator.
    pub(super) fn peek_type_op_continuation(&self) -> Option<(SyntaxKind, SyntaxKind, u16)> {
        let (res, _) = self.peek()?;
        let (node_kind, op_tok, lbp) = match res {
            Ok(FilteredToken::Raw(Token::ColonQMark)) => {
                (SyntaxKind::TYPE_TEST_EXPR, SyntaxKind::COLON_QMARK_TOK, 55)
            }
            Ok(FilteredToken::Raw(Token::ColonGreater)) => {
                (SyntaxKind::UPCAST_EXPR, SyntaxKind::COLON_GREATER_TOK, 25)
            }
            Ok(FilteredToken::Raw(Token::ColonQMarkGreater)) => (
                SyntaxKind::DOWNCAST_EXPR,
                SyntaxKind::COLON_QMARK_GREATER_TOK,
                25,
            ),
            _ => return None,
        };
        // Swallowed-closer gate (same as `peek_infix_continuation`): if the next
        // non-trivia raw token is a LexFilter-stripped `)` / `}`, the operator
        // belongs to the enclosing expression.
        for (res, _) in self.raw_tokens.iter().skip(self.raw_pos) {
            match res {
                Ok(t) if raw_is_trivia(t) => continue,
                Ok(TriviaToken::Lexed(Token::RParen | Token::RBrace)) => return None,
                _ => break,
            }
        }
        // Swallowed-closer-*after* gate: a cast operator immediately before a
        // swallowed closer (`(x :>) T`) has its RHS type stripped from the
        // filtered stream — leave it for enclosing recovery rather than reaching
        // across the closer (mirrors `peek_infix_continuation` /
        // `peek_cons_continuation`).
        if let Some((_, op_span)) = self.peek()
            && self.op_rhs_is_swallowed_closer(op_span.end)
        {
            return None;
        }
        Some((node_kind, op_tok, lbp))
    }

    /// `Some((lbp, rbp))` if the next filtered token is the cons operator `::`
    /// (`Token::ColonColon`) the Pratt climber should pick up into a
    /// [`SyntaxKind::CONS_EXPR`], `None` otherwise.
    ///
    /// `::` is **not** a `mkSynInfix` operator: FCS's `declExpr COLON_COLON
    /// declExpr` (`pars.fsy:4765`) builds a *single* `App(NonAtomic,
    /// isInfix = true, op_ColonColon, Tuple(false, [lhs; rhs]))` — the operator
    /// applied to a synthesised pair — rather than the two-tier `App(App(op,
    /// lhs), rhs)` shape, so it cannot ride [`Parser::peek_infix_op`]. Like the
    /// type-relation operators (`:?` / `:>` / `:?>`) it is a distinct
    /// continuation, handled in [`Parser::parse_pratt_expr`]'s loop.
    ///
    /// Right-associative (`%right COLON_COLON`, `pars.fsy:361`), so `lbp == rbp`:
    /// the recursive RHS keeps consuming further `::`s into one right-leaning
    /// chain (`a :: b :: c` ⇒ `a :: (b :: c)`). Precedence band `45` sits between
    /// `@`/`^` (INFIX_AT_HAT_OP, lbp `40`, `:360` — looser) and `:?` (COLON_QMARK
    /// type-test, lbp `55`, `:363`) / `+`/`-` (PLUS_MINUS, lbp `60`, `:364` —
    /// tighter), matching FCS's table.
    ///
    /// Applies the swallowed-`)`/`}` gate of [`Parser::peek_infix_continuation`]
    /// on **both sides** of the operator plus the RHS-must-exist gate:
    ///
    /// * **Closer *before* `::`** (operator belongs to the enclosing frame):
    ///   inside `( e )` / `{ e }` the closer is stripped from the filtered
    ///   stream, so a `::` after the body's swallowed closer surfaces as the
    ///   body's immediate filtered successor — `(a) :: b` must be
    ///   `App(::, [Paren a, b])`, not `Paren(App(::, [a, b]))`. The raw scan from
    ///   the cursor catches the still-pending closer.
    ///
    /// * **Closer *after* `::`** (the operator's RHS is missing): on incomplete
    ///   input the `::`'s own RHS may be a LexFilter-swallowed `)` / `}` (e.g.
    ///   `(a ::) y`). Because that closer is stripped from the filtered stream,
    ///   the RHS-must-exist check below ([`Parser::is_expr_start_at`], a filtered
    ///   lookahead) would peer *past* it and wrongly see the enclosing token
    ///   (`y`) as the tail — building the cons across the closer and draining the
    ///   real `)` as `ERROR`. The raw lookahead *after the operator's span*
    ///   ([`Parser::next_non_trivia_raw_after`]) catches this: a closer
    ///   immediately after `::` means no RHS, so leave the `::` for enclosing
    ///   recovery (lossless, no mis-nesting). Mirrors the pattern-cons recovery
    ///   path ([`Parser::emit_pat_atom`]'s raw closer reject).
    ///
    /// * **RHS-must-exist**: a trailing `::` with no following operand is left
    ///   for enclosing recovery — lossless, no panic, matching how a dangling
    ///   `+` is handled by [`Parser::peek_infix_continuation`].
    pub(super) fn peek_cons_continuation(&self) -> Option<(u16, u16)> {
        let op_span = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::ColonColon)), span)) => span.clone(),
            _ => return None,
        };
        // Swallowed-closer-*before* gate: a closer pending at the cursor means
        // the `::` belongs to the enclosing frame.
        for (res, _) in self.raw_tokens.iter().skip(self.raw_pos) {
            match res {
                Ok(t) if raw_is_trivia(t) => continue,
                Ok(TriviaToken::Lexed(Token::RParen | Token::RBrace)) => return None,
                _ => break,
            }
        }
        // Swallowed-closer-*after* gate: a closer immediately following the `::`
        // (in the raw stream, past the filtered-stripped one) is not an RHS —
        // leave the `::` for recovery rather than reaching across it.
        if self.op_rhs_is_swallowed_closer(op_span.end) {
            return None;
        }
        // RHS-must-exist gate.
        if !self.is_expr_start_at(self.pos + 1) {
            return None;
        }
        Some((45, 45))
    }

    /// `Some((lbp, rbp))` if the next filtered token is the query
    /// computation-expression join operator — LexFilter's `Virtual::JoinIn`
    /// rewrite of an `in` inside a brace CE (`detect_join_in_ctxt`,
    /// `crate::lexfilter`) — `None` otherwise.
    ///
    /// Left-associative at the `||`/`or` precedence band (`%left OR BAR_BAR
    /// JOIN_IN`, `pars.fsy:352`), so `lbp == 10`, `rbp == 11` — the same band
    /// as [`Parser::peek_infix_op`]'s `Token::BarBar`. Unlike the `mkSynInfix`
    /// operators it lowers to a dedicated [`SyntaxKind::JOIN_IN_EXPR`] node
    /// (`SynExpr.JoinIn`, `declExpr JOIN_IN declExpr`, `pars.fsy:4669`), so it
    /// is picked up directly in [`Parser::parse_pratt_expr`]'s loop, beside the
    /// cons and type-relation continuations.
    ///
    /// One **closer-before** gate is applied (the RHS-side gate lives at the
    /// consumption site in [`Parser::parse_pratt_expr`]'s loop): when the join
    /// LHS ends in a parenthesised / braced sub-expression whose `)` / `}` is
    /// LexFilter-swallowed (`query { f(a) in xs }`), the `Virtual::JoinIn`
    /// surfaces as the *immediate filtered successor* of the inner paren body
    /// while the raw cursor still sits on the swallowed closer. Without the gate
    /// the recursive `parse_expr` *inside* the paren would take the join,
    /// draining the real `)` as an error and mis-nesting it as
    /// `f(Paren(JoinIn(a, xs)))` instead of `JoinIn(App(f, Paren a), xs)`. The
    /// raw scan from the cursor catches the still-pending closer, so the inner
    /// frame declines and the enclosing frame (after the `)` is recovered) takes
    /// the join. Same gate as [`Parser::peek_cons_continuation`]'s closer-before
    /// check. A missing RHS (the incomplete `query { x in }` shape, FCS's
    /// `declExpr JOIN_IN ends_coming_soon_or_recover`, `pars.fsy:4672`) is
    /// handled at the consumption site by a missing-operand error.
    pub(super) fn peek_join_in_continuation(&self) -> Option<(u16, u16)> {
        if !matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::JoinIn)), _))
        ) {
            return None;
        }
        // Swallowed-closer-*before* gate: a `)` / `}` pending at the cursor means
        // the join belongs to the enclosing frame, not this (inner paren-body)
        // one.
        for (res, _) in self.raw_tokens.iter().skip(self.raw_pos) {
            match res {
                Ok(t) if raw_is_trivia(t) => continue,
                Ok(TriviaToken::Lexed(Token::RParen | Token::RBrace)) => return None,
                _ => break,
            }
        }
        Some((10, 11))
    }

    /// Consume the [`Virtual::JoinIn`] at the cursor, emitting its backing raw
    /// `Token::In` as an [`SyntaxKind::IN_TOK`]. LexFilter leaves the raw `in`
    /// in the stream at the virtual's span — a *backed-by-raw* relabel like
    /// [`Virtual::With`] — so this drains preceding trivia, emits the keyword
    /// text, and advances both the filtered and raw cursors past it (the
    /// `WITH_TOK` / `THEN_TOK` emission pattern).
    pub(super) fn emit_join_in_token(&mut self) {
        let Some((Ok(FilteredToken::Virtual(Virtual::JoinIn)), span)) = self.peek().cloned() else {
            unreachable!("emit_join_in_token invoked without a peeked Virtual::JoinIn");
        };
        self.drain_raw_up_to(span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::In)), s)) if *s == span,
            ),
            "Virtual::JoinIn must be backed by a raw Token::In at raw_pos with matching span"
        );
        self.emit_text(SyntaxKind::IN_TOK, span);
        self.raw_pos += 1;
        self.pos += 1;
    }

    /// FCS LexFilter rule at `SyntaxTree/LexFilter.fs:2694`: when the
    /// current operator is `MINUS | PLUS_MINUS_OP "+"/"+."/"-." |
    /// PERCENT_OP "%"/"%%" | AMP | AMP_AMP`, AND it's adjacent to the
    /// next token (no whitespace between), AND it has a *gap* from the
    /// preceding token (whitespace OR was not at an atomic end — the
    /// rule is `not (prevWasAtomicEnd && lastTokenPos == startOfThis)`),
    /// the token is rewritten to `ADJACENT_PREFIX_OP` and the
    /// integer-literal branch may even fold the sign into the literal.
    ///
    /// For phase 3.4 we just need to *avoid* eating these as infix —
    /// phase 3.5 will add the prefix-form parsing. We bail before the
    /// Pratt climber takes them.
    ///
    /// Concrete examples:
    /// * `f -1`  — gap-left `-`, no-gap-right → prefix → bail.
    /// * `1 +2`  — gap-left `+`, no-gap-right → prefix → bail.
    /// * `f - 1` — gap on both sides → still infix → don't bail.
    /// * `f-1`   — no gap on either side → still infix → don't bail.
    ///   (FCS keeps this as plain `MINUS`, infix.)
    /// * `(-1)`  — `-` is the *first* token of the paren body, no LHS
    ///   atomic end → still hits the prefix rule. Phase 3.4 won't see
    ///   this case anyway (no LHS to attach infix to).
    pub(super) fn op_is_adjacent_prefix(&self) -> bool {
        let (res, op_span) = self.peek().expect("caller already peeked an op");
        let tok = match res {
            Ok(FilteredToken::Raw(t)) => t,
            _ => return false,
        };
        // FCS's exact eligible set (`LexFilter.fs:2694`). Single `-`
        // lexes as `Op("-")` (we have no dedicated MINUS variant); `&` is
        // `Token::Amp`, `&&` is `Token::AmpAmp`. Other PLUS_MINUS_OP
        // shapes (`-_op`, `-/`, etc.) are explicitly excluded by FCS's
        // match-arm carve-out and don't get rewritten. Note that
        // `Token::Amp` reaches this predicate only from arg-position
        // checks ([`Parser::at_app_continuation`]) — `Token::Amp` is
        // *not* a Pratt infix (no `declExpr AMP declExpr` rule in FCS),
        // so [`Parser::peek_infix_continuation`] never reaches this
        // function with an `&`.
        let eligible = match tok {
            Token::Op(text) => matches!(*text, "-" | "+" | "+." | "-." | "%" | "%%"),
            Token::Amp | Token::AmpAmp => true,
            _ => false,
        };
        if !eligible {
            return false;
        }
        // Adjacent-right: the next *real* raw token after the op (skip
        // trivia: whitespace, newlines, comments) must start exactly at
        // `op_span.end`. Find the op's raw token first, then walk
        // forward.
        // Binary-search to the op's raw token (raw spans are sorted and
        // contiguous) rather than scanning from index 0. This predicate runs
        // once per infix operator during Pratt parsing, and `op_span.start`
        // advances monotonically through the file, so a from-zero `.position()`
        // is O(position) per call — O(n²) over a long operator chain. Spans
        // are unique by start, so the only candidate for an exact match is the
        // first token starting at-or-after `op_span.start`; require the exact
        // span the old `.position()` did (rejecting a split's sub-range).
        let cand = self
            .raw_tokens
            .partition_point(|(_, span)| span.start < op_span.start);
        let op_raw_idx = self
            .raw_tokens
            .get(cand)
            .filter(|(_, span)| span.start == op_span.start && span.end == op_span.end)
            .map(|_| cand);
        let Some(idx) = op_raw_idx else {
            return false;
        };
        let next_real_start =
            self.raw_tokens
                .iter()
                .skip(idx + 1)
                .find_map(|(res, span)| match res {
                    Ok(t) if raw_is_trivia(t) => None,
                    Ok(_) | Err(_) => Some(span.start),
                });
        let adjacent_right = matches!(next_real_start, Some(start) if start == op_span.end);
        if !adjacent_right {
            return false;
        }
        // Non-adjacent-left: there is whitespace (or any trivia) before
        // the op, OR no previous real token. FCS phrases this as
        // `not (prevWasAtomicEnd && lastTokenPos == startOfThisToken)`
        // — a non-atomic previous would also satisfy it, but phase 3.4
        // only gets here after parsing an LHS expression atom, so any
        // real preceding raw token is at an atomic boundary. Reduces to
        // "gap before the op, or no preceding real token."
        let prev_real_end = self.raw_tokens[..idx]
            .iter()
            .rev()
            .find_map(|(res, span)| match res {
                Ok(t) if raw_is_trivia(t) => None,
                Ok(_) => Some(span.end),
                Err(_) => None,
            });
        match prev_real_end {
            Some(end) => end < op_span.start,
            None => true,
        }
    }

    /// `peek_is_expr_start` but at an arbitrary positive offset into the
    /// filtered stream rather than always at the cursor. Used by
    /// [`Parser::peek_infix_continuation`] to peer past the operator at
    /// the cursor and decide whether an expression atom follows.
    pub(super) fn is_expr_start_at(&self, offset: usize) -> bool {
        match self.filtered_tokens.get(offset) {
            // `LParen` as atom-starter must look past the swallowed
            // `RParen` (same logic as `peek_is_expr_start`). RHS of an
            // infix operator parses through `parse_pratt_expr` →
            // `parse_minus_expr`, so the minus-level set is the right
            // gate here too — the shared `(`-after predicate
            // ([`raw_after_lparen_starts_expr`], incl. a block `let`/`use`).
            Some((Ok(FilteredToken::Raw(Token::LParen)), lparen_span)) => self
                .next_non_trivia_raw_after(lparen_span.end)
                .is_some_and(raw_after_lparen_starts_expr),
            Some((Ok(FilteredToken::Raw(t)), _)) if raw_starts_minus_expr(t) => true,
            // A non-block `let … in` operand surfaces as a raw `Token::Let` in
            // the filtered stream (block-leading lets are `Virtual(Let)` instead).
            // It is deliberately *not* in `raw_starts_minus_expr` (that classifier
            // is also read on the raw stream in decl context, where a module-level
            // `let` is a raw `Token::Let`), so admit it here so the infix RHS gate
            // consumes the operator (`a && let x = e in b`). `parse_minus_expr`
            // dispatches it. (`use` is excluded — see that dispatch's note.)
            Some((Ok(FilteredToken::Raw(Token::Let)), _)) => true,
            // `_.member` — the accessor-function shorthand as an infix RHS
            // (`x + _.Foo`). Kept symmetric with `peek_is_expr_start`'s arm; the
            // `_.` two-token shape is checked at this offset rather than the
            // cursor (the operator sits between the cursor and `offset`).
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _)) if self.at_dot_lambda(offset) => {
                true
            }
            // `?ident` — the optional-named-argument expression as an infix RHS
            // (`x + ?opt`). An `atomicExpr`, kept symmetric with the `_.` arm;
            // checked at `offset` (the operator sits between the cursor and
            // `offset`).
            Some((Ok(FilteredToken::Raw(Token::QMark)), _))
                if self.qmark_opens_optional_arg_at(offset) =>
            {
                true
            }
            // The whole-dimension wildcard `*` is a high-precedence atom
            // (`parse_index_wildcard`), so it is a valid infix RHS — `1 + *` is
            // `App(+, 1, IndexRange(None,None))`, `* * *` chains. Mirrors the
            // `peek_is_expr_start` arm. (A leading `..` is deliberately *not*
            // mirrored here: it is a looser-than-Pratt `declExpr` leaf, so an
            // infix RHS `1 + ..3` stays the documented clean-error deferral.)
            Some((Ok(FilteredToken::Raw(Token::Op("*"))), _)) => true,
            // `^expr` from-end prefix as an infix RHS — `1 + ^1` is `App(+, 1,
            // IndexFromEnd 1)` in FCS (the `+` operand is a `minusExpr`, which
            // `^expr` is). Mirrors the `peek_is_expr_start` arm.
            Some((Ok(FilteredToken::Raw(Token::Op("^"))), _)) => true,
            // Virtual rewrite of `Token::Fun` — kept symmetric with the
            // `peek_is_expr_start` arm so an infix RHS like `1 + fun x -> x`
            // (should it ever be valid input) classifies the same way.
            Some((Ok(FilteredToken::Virtual(Virtual::Fun)), _)) => true,
            // `Virtual::Let` — a *block-leading* `let … in` as the infix RHS,
            // `a &&⏎    let y = x⏎    y` (the `&&`-at-end-of-line layout, where
            // LexFilter promotes the next-line `let` to the virtual rather than
            // the raw `Token::Let` of the single-line form). Kept symmetric with
            // `peek_is_expr_start`'s `Virtual::Let` arm; `parse_minus_expr`
            // dispatches it to `parse_let_or_use_expr`.
            Some((Ok(FilteredToken::Virtual(Virtual::Let)), _)) => true,
            // Virtual rewrite of `Token::Function` (`OFUNCTION`) — same
            // symmetry as `Virtual::Fun`.
            Some((Ok(FilteredToken::Virtual(Virtual::Function)), _)) => true,
            // `Virtual::Lazy`/`Virtual::Assert` (`OLAZY`/`OASSERT`) — an
            // offside/control-flow `lazy`/`assert` as the infix/cons RHS, e.g.
            // `a ||⏎ lazy⏎ b` or `a || lazy if c then d else e`. Kept symmetric
            // with `peek_is_expr_start`'s arm; `parse_minus_expr` dispatches it to
            // `parse_lazy_or_assert`.
            Some((Ok(FilteredToken::Virtual(Virtual::Lazy | Virtual::Assert)), _)) => true,
            _ => false,
        }
    }

    /// Classify the current filtered token as an infix operator,
    /// returning `(lbp, rbp)`. Left-associative ops have `rbp = lbp - 1`
    /// (lower-right-binding lets the outer iteration win at equal
    /// precedence); right-associative ops have `rbp = lbp` (recursion
    /// keeps going at equal precedence, building a right-leaning tree).
    ///
    /// Precedence levels mirror FCS's `pars.fsy` table (ascending):
    /// COLON_EQUALS, BAR_BAR, AMP/AMP_AMP, the compare bucket
    /// (INFIX_COMPARE_OP / EQUALS / LESS / GREATER / DOLLAR /
    /// INFIX_BAR_OP / INFIX_AMP_OP), INFIX_AT_HAT_OP, COLON_COLON,
    /// PLUS_MINUS_OP, INFIX_STAR_DIV_MOD_OP, INFIX_STAR_STAR_OP,
    /// QMARK_QMARK. Operators FCS classifies as semantically distinct
    /// from `mkSynInfix` (e.g. `<-` assignment, `..` range, the `or` / `and` /
    /// `join_in` keywords) are deliberately *not* listed here — this classifier
    /// only covers ops that lower to the two-tier `mkSynInfix` `App` shape. The
    /// type-relation operators `:>` / `:?>` upcast/downcast and `:?` typetest are
    /// distinct nodes, picked up in [`Parser::parse_pratt_expr`]'s loop via
    /// [`Parser::peek_type_op_continuation`] rather than here; the cons operator
    /// `::` (COLON_COLON) likewise — FCS lowers it to a single
    /// `App(op, Tuple([lhs; rhs]))`, so it rides
    /// [`Parser::peek_cons_continuation`] into a [`SyntaxKind::CONS_EXPR`].
    pub(super) fn peek_infix_op(&self) -> Option<(u16, u16)> {
        let (res, _) = self.peek()?;
        let tok = match res {
            Ok(FilteredToken::Raw(t)) => t,
            _ => return None,
        };
        match tok {
            Token::Op(s) => classify_op_text(s),
            // The compare bucket. `Less(true)` / `Greater(true)` are the
            // typar-bracket form (LexFilter promotes the bool); only the
            // `false` variant is a binary compare.
            Token::Equals => Some((30, 31)),
            Token::Less(false) => Some((30, 31)),
            Token::Greater(false) => Some((30, 31)),
            Token::Dollar => Some((30, 31)),
            // `&&` is INFIX_AMP-style boolean conjunction (pars.fsy:355
            // `%left AMP AMP_AMP`). Bare `Token::Amp` is *intentionally
            // absent* — pars.fsy has no `declExpr AMP declExpr` rule;
            // single `&` is only the address-of *prefix* (line 5162
            // `AMP minusExpr`) and the conjunction binder in patterns
            // (line 3650-3653). Phase 3.5 will handle the prefix form.
            Token::AmpAmp => Some((20, 21)),
            Token::BarBar => Some((10, 11)),
            // `??` (`QMARK_QMARK`) is *intentionally absent*: pars.fsy
            // declares it only in `%token` (line 88) and `%left
            // QMARK_QMARK` (line 367) for precedence; there is NO
            // `declExpr QMARK_QMARK declExpr` production. FCS rejects
            // `a ?? b` with "Unexpected symbol '??'". Listing it here
            // would normalise an oracle-rejected input. Phase 3.4's
            // earlier inclusion was a misread of the precedence-table
            // line: the precedence levels don't imply infix productions.
            // `mod` is the keyword spelling of `INFIX_STAR_DIV_MOD_OP`
            // (lex.fsl line 972 covers the symbolic `* / %` variants;
            // `mod` is dispatched the same way in pars.fsy). Same
            // precedence band as `*` and `/`, left-associative.
            Token::Mod => Some((70, 71)),
            // `::` (`COLON_COLON`) is *intentionally absent* here, but for a
            // shape reason, not a precedence one: pars.fsy:4765 special-cases it
            // to `App(NonAtomic, isInfix=true, op, Tuple([lhs;rhs]))` — a single
            // App whose arg is a synthesised pair, not the two-tier `mkSynInfix`
            // shape this classifier emits. So it gets a dedicated
            // [`SyntaxKind::CONS_EXPR`] node, picked up in
            // [`Parser::parse_pratt_expr`]'s loop via
            // [`Parser::peek_cons_continuation`] (beside the type-relation
            // operators), not here.
            //
            // `:=` (`COLON_EQUALS`) is *also* absent here, but for a
            // precedence reason, not a shape one: pars.fsy:344 places it
            // *below* COMMA (line 346), so its operands are whole tuples
            // (`r := a, b` is `r := (a, b)`). The Pratt climber runs *under*
            // the tuple loop, so `:=` is handled one level *above* it in
            // [`Parser::parse_expr`] (the `mkSynInfix` shape is identical to
            // an ordinary infix op — it just binds looser than the comma).
            _ => None,
        }
    }

    /// Emit the current filtered token (the operator) as
    /// `LONG_IDENT_EXPR > LONG_IDENT > IDENT_TOK("<src>")`, the same
    /// shape FCS's `mkSynOperator` produces. FCS additionally mangles
    /// the source text into `Ident.idText = "op_Addition"` and stashes
    /// the original in `IdentTrivia.OriginalNotation "+"`; we keep the
    /// source text directly in the green-tree token (matching the
    /// `OriginalNotation` channel) and let the FCS-side normaliser
    /// unwrap `OriginalNotation` so the two sides diff cleanly.
    pub(super) fn emit_infix_op_as_long_ident(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_EXPR));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        self.bump_into(SyntaxKind::IDENT_TOK);
        self.builder.finish_node();
        self.builder.finish_node();
    }

    /// `pars.fsy:5258 atomicExpr: PREFIX_OP atomicExpr` — the `!`-headed
    /// or `~`-headed prefix operator. Emits
    /// `APP_EXPR > [LONG_IDENT_EXPR(op), <operand>]`, matching FCS's
    /// `mkSynPrefixPrim` output (`SyntaxTreeOps.fs:482`). The operand is
    /// parsed at atomic level (`PREFIX_OP atomicExpr`, NOT `minusExpr`),
    /// so `! - x` is *not* `! (-x)` — the inner `-` would have to be in
    /// arg/atomic position, where it cannot stand alone. Sign-folded /
    /// minus-level prefixes belong to [`Parser::parse_minus_expr`] one
    /// level up.
    pub(super) fn parse_prefix_op_app(&mut self) {
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
}
