//! Pattern productions: head-binding patterns, the precedence-climbing
//! pattern tail (`as` / `,` / `:` / `::`), and the atomic-pattern forms
//! (const, paren/tuple, array/list, record, `:?` type-test).

use super::*;

/// The grammar context a pattern element / tail-climb runs in. FCS splits the
/// pattern grammar into `headBindingPattern` (the bare `let`/`fun` head) and
/// `parenPattern` (everything inside delimiters and at `match`/`function`
/// clause heads); the two differ in which features an element admits. This
/// three-way context selects them precisely — a single `in_paren` bool can't,
/// because a clause head is a `parenPattern` reached *outside* any delimiter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PatCtx {
    /// Bare `let`/`fun` head — `headBindingPattern`. No per-element `:`
    /// typed-pat (a top-level `:` is `SynBinding.returnInfo`) and **no**
    /// attributes (`let x, [<A>] y` is an FCS parse error).
    Head,
    /// Inside parens / list / array / record-field — a full `parenPattern`.
    /// Per-element `:` typed-pat and `[<…>]` attributes are both admitted.
    Paren,
    /// A `match` / `function` / `match!` clause head — a `parenPattern` reached
    /// outside any delimiter. `[<…>]` attributes are admitted (every clause
    /// operand is a `parenPattern`), but the per-element `:` is left to the
    /// (erroring) FCS `parenPattern COLON` path, as at the head.
    Clause,
}

/// Which `opName` ends a dotted pattern path — see
/// [`Parser::peek_dotted_opname_pat_head`]. FCS's `pathOp: ident DOT pathOp` with
/// a final `pathOp: opName` (`pars.fsy:6930`) lets *any* pattern long-ident end in
/// a parenthesised operator (`A.B.(+)`) or an active-pattern name
/// (`A.B.(|Foo|_|)`); the member self-id head (`member x.(+)`) is one instance of
/// it. The operator form distinguishes the glued `(*)` token (`is_star`) from the
/// general / spaced `( op )`.
#[derive(Clone, Copy)]
pub(super) enum DottedOpNameHead {
    Operator { is_star: bool },
    ActivePat,
}

impl<'src> Parser<'src> {
    /// `pars.fsy:5006 headBindingPattern` — the LHS of a single
    /// `localBinding`. Returns `true` on success (a `NAMED_PAT`,
    /// `WILDCARD_PAT`, or `LONG_IDENT_PAT` was emitted); `false` if the
    /// head wasn't a recognised pattern-start token (in which case an
    /// error has been pushed and no node was opened).
    ///
    /// Phase 4.5 picks between three shapes:
    ///
    /// - **Value form / ident** (`let x = e`): single-ident head whose
    ///   one-token lookahead is *not* another atomic-pat start. Emits
    ///   `NAMED_PAT > [IDENT_TOK]`, matching FCS's `SynPat.Named`.
    ///
    /// - **Value form / wildcard** (`let _ = e`): head token is `_`.
    ///   Emits `WILDCARD_PAT > [UNDERSCORE_TOK]`, matching FCS's
    ///   `SynPat.Wild`. FCS does *not* promote wildcard heads to
    ///   function form — `let _ x = e` is a parser error in FCS
    ///   (`SynPat.Wild` head + FS0010 on the trailing ident), so we
    ///   short-circuit before the function-form decision.
    ///
    /// - **Function form** (`let f x _ y = e`): ident head with at
    ///   least one atomic-pat-start token following. Emits
    ///   `LONG_IDENT_PAT > [LONG_IDENT > IDENT_TOK(head), <arg-pat>+]`,
    ///   matching FCS's `SynPat.LongIdent(_, _, _, SynArgPats.Pats […],
    ///   _, _)`. Each arg is itself a [`SyntaxKind::NAMED_PAT`] or
    ///   [`SyntaxKind::WILDCARD_PAT`] (parens / tuples / typed args
    ///   arrive later).
    ///
    /// The lookahead remains intentionally one-token: more sophisticated
    /// arg shapes will need a real `parse_atomic_pat`, but the
    /// flat ident-or-wildcard `while` loop covers the common case and
    /// keeps the diff small.
    pub(super) fn parse_head_binding_pat(&mut self) -> bool {
        // Checkpoint *before* the first element so a trailing `,` lets us
        // retroactively wrap the whole head in `TUPLE_PAT` (phase 6.3).
        // Mirrors FCS's `headBindingPat → applPats (',' applPat)+`
        // structure where each tuple element is itself an applPat
        // (function-form longident or atomic), not atomic-only.
        let tuple_cp = self.builder.checkpoint();

        if !self.try_emit_head_binding_pat_element() {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected pattern after `let`".to_string(),
                span,
            });
            return false;
        }

        // Wrap the head in any tuple / `as` operators that follow, in token
        // order. `PatCtx::Head`: a top-level `let … : t = …` colon is
        // `SynBindingReturnInfo`, not a typed-pat (colon arm inert), and a bare
        // head admits no attributes (`headBindingPattern` has no `attributes`
        // production).
        self.wrap_pat_tail(tuple_cp, PatCtx::Head);
        true
    }

    /// Emit one element of a tuple-pattern head (or the whole head when
    /// no comma follows): function-form `Ctor arg1 arg2` if the cursor
    /// is at an ident with an atomic-pat start following, otherwise a
    /// plain atomic pattern. Returns `false` (without emitting or
    /// consuming) when the cursor isn't at an atomic-pat start at all
    /// — callers push their own context-specific error.
    ///
    /// FCS promotes ident heads (`let f x = …`) and parenthesised
    /// operator-name heads (`let (+) a b = …` — FCS's `opName`, handled by the
    /// `at_paren_op_value` branch below) to function form; other non-ident heads
    /// (wildcard, a paren-*pattern* `(x)`, unit, null, const literal) stay
    /// value-form even when followed by what looks like a curried arg — any
    /// trailing token becomes a parser error at the binding-tail level. Wildcard
    /// heads have an explicit test for this (`let _ x = e`); the other shapes
    /// inherit the same rule.
    ///
    /// Used both for the first element of the head and for each
    /// continuation element inside `maybe_wrap_tuple_pat`, so
    /// `let y, Some x = …` and `let Some x, y = …` produce the same
    /// per-element shapes that FCS does (`headBindingPat → applPats
    /// (',' applPat)+` recurses through applPat on both sides).
    pub(super) fn try_emit_head_binding_pat_element(&mut self) -> bool {
        // `:? atomTypeOrAnonRecdType` — `SynPat.IsInst`. FCS places this at the
        // `constrPattern` level (`pars.fsy:3729`), one rung above the atomic
        // patterns, so it is handled here rather than in `try_emit_atomic_pat`.
        // This single hook covers all the constrPattern caller sites: the
        // match-clause pattern, the `let` head, and parenthesised elements
        // (`(:? T)`, reached via `emit_paren_pat_element`). A bare `fun :? T`
        // is correctly *not* covered — `parse_fun_expr` parses atomic-only
        // args, so the IsInst there only parses inside parens, matching FCS.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::ColonQMark)), _))
        ) {
            self.parse_is_inst_pat();
            return true;
        }

        // Active-pattern name head — `(|Foo|Bar|)`, `(|Foo|_|)`. Detected before
        // the atomic-pat dispatch so the `(` is consumed as the active-pattern
        // name rather than as an ordinary paren pattern (whose `|`-led body
        // would be a parse error). The *un*-qualified case is taken here; the
        // access-modified case (`let private (|Foo|Bar|) …`) is taken by the
        // second call below, after the modifier is consumed.
        if self.try_emit_active_pat_head() {
            return true;
        }

        // Accessibility on the head pattern (FCS's `atomicPatternLongIdent:
        // access pathOp`, `pars.fsy:2279`): a leading `private`/`internal`/
        // `public` before a `pathOp` — an identifier-path (`let private x = …`,
        // `let internal f a = …`) *or* a parenthesised operator name
        // (`let private (+) x = …`; `opName` is a `pathOp` alternative). FCS
        // attaches the modifier to the resulting `SynPat.Named` (field 2) /
        // `SynPat.LongIdent` (field 4) accessibility slot, both elided by the
        // normaliser; we consume it as a sibling `ACCESS_TOK` (mirroring the
        // exception / union-case / record-field access sites — a direct token
        // child, invisible to the node-based pattern projection). The grammar
        // permits access only before a `pathOp`, so `private _` / `private (x)`
        // (a paren *pattern*, not a `pathOp`) / `private 1` have no access
        // production and stay clean errors. (The rooted `global.N` head is now a
        // recognised pattern — see [`Self::pat_head_has_dotted_tail`] — but the
        // *access-prefixed* form `access GLOBAL DOT pathOp` stays an edge: this
        // gate keys on an ident head, so `private global.N` isn't consumed here.
        // `_.M` and its access-prefixed form remain deferred behind the F# 4.7
        // gate.) The gate consults *both* streams, like the
        // function-form promotion below: the **raw** lookahead (past the keyword
        // span) rejects a LexFilter-swallowed `)` — in `let f (private) x = …`
        // the inner `private`'s raw-after is the `)`, not the *outer* `x` the
        // filtered stream skips to, so without it we would consume `private` and
        // steal `x` into the parens; the **filtered** lookahead rejects an
        // offside-separated name (a `Virtual::BlockSep` between keyword and
        // ident). The consumed access leaves the cursor at the head, so the
        // active-pattern / operator-head dispatch and the `is_atomic_pat_start`
        // check / function-form promotion below proceed exactly as for an
        // unqualified head.
        //
        // `pathOp` also covers active-pattern operator names (`let private
        // (|Foo|Bar|) …`, keyed on a following `( |`) and parenthesised operator
        // names (`let private (+) x = …` / the glued `(*)`, keyed on the head's
        // own `(`/`(*)` opener — *not* a swallowed enclosing `)`). The
        // active-pattern head is parsed by the second `try_emit_active_pat_head`
        // call below; the operator head by the dispatch after it.
        if let Some((
            Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
            span,
        )) = self.peek().cloned()
        {
            let raw_after = self.next_non_trivia_raw_after(span.end);
            let before_ident =
                matches!(
                    self.next_non_trivia_filtered_after_pos(),
                    Some(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_)))
                ) && matches!(raw_after, Some(Token::Ident(_) | Token::QuotedIdent(_)));
            // Active-pattern name head after the modifier (`let private
            // (|Foo|Bar|) …`) — keyed on a following `( |` opener.
            let before_active_pat = matches!(
                self.next_non_trivia_filtered_after_pos(),
                Some(FilteredToken::Raw(Token::LParen))
            ) && self.raw_active_pat_name_starts_after(span.end);
            // Operator-name head after the modifier — the raw lookahead must be
            // the head's own `(`/`(*)` opener (rejecting the swallowed-`)`
            // hazard), and the filtered position must form an operator value.
            let op_target = matches!(raw_after, Some(Token::LParen | Token::LParenStarRParen))
                && self
                    .next_non_trivia_filtered_index_after(self.pos)
                    .is_some_and(|j| {
                        matches!(
                            self.filtered_tokens.get(j),
                            Some((Ok(FilteredToken::Raw(Token::LParenStarRParen)), _))
                        ) || self.at_paren_op_value_pat(j)
                    });
            if before_ident || before_active_pat || op_target {
                self.bump_into(SyntaxKind::ACCESS_TOK);
            }
        }

        // Access-modified active-pattern name (`let private (|Foo|Bar|) …`): the
        // modifier (if any) has been consumed above, so re-run the recogniser
        // here, now that the cursor sits at the `(`.
        if self.try_emit_active_pat_head() {
            return true;
        }

        // Operator-name binding head — FCS's `opName`, reached through `pathOp →
        // atomicPatternLongIdent`. Three lexical spellings, unified here:
        //   * the glued `(*)` multiply token ([`Token::LParenStarRParen`],
        //     `opName: LPAREN_STAR_RPAREN`) — `is_star = true`;
        //   * the general `( op )` (`( + )`, `(>>>&)`, …) and the spaced
        //     `( * )` — both `is_star = false` ([`Self::at_paren_op_value_pat`]
        //     admits the star in pattern position; see its doc).
        // The head composes with the same `[head] [typars] [args]` machinery as
        // an ident head, so it routes to `SynPat.Named` when nullary (no typars,
        // no args — the `atomicPattern` reduction) and `SynPat.LongIdent` when
        // typars and/or curried args follow (the `constrPattern` reduction,
        // `pars.fsy:3689`/`3711`). This dispatch runs *before* the
        // `is_atomic_pat_start` gate because the glued `(*)` token is not an
        // atomic-pat start; the general `( op )` would pass the gate but is
        // handled here too so all three spellings share one path.
        if let Some(is_star) = self.peek_operator_head() {
            let (after, raw_after) = self.operator_head_after(is_star);
            // Explicit value-typar decls (`let (!!)<'T> … `) — a `<` adjacent
            // (`HighPrecedenceTyApp`) or spaced (`Less`) right after the head's
            // close. Same detection as the ident head below; always forces the
            // `LongIdent` form (even with zero args).
            let has_typars = matches!(
                after,
                Some(
                    FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                        | FilteredToken::Raw(Token::Less(_))
                )
            );
            let has_args = Self::op_head_args_follow(after, raw_after);
            self.emit_operator_head(is_star, has_typars, has_args);
            return true;
        }

        // A dotted path whose *final* segment is an `opName` — `A.B.(+) y`,
        // `A.B.(|Foo|_|) y`, `global.A.(*)` (FCS's `pathOp` ending in an `opName`,
        // folded into one `SynLongIdent`: `["A"; "B"; "op_Addition"]` /
        // `["A"; "B"; "|Foo|_|"]`). The *un*-dotted spellings are taken by the
        // operator / active-pattern head dispatches above (their head is the `(`);
        // a dotted one has an ident / `global` head, so it falls through to here —
        // ahead of the ident dispatch below, whose long-ident sweep admits only
        // ident segments after a `.` and would report a trailing dot.
        // `allow_underscore_head = false`: the `_.`-rooted path stays deferred
        // behind its F# 4.7 gate (see [`Self::peek_dotted_opname_pat_head`]).
        if let Some(kind) = self.peek_dotted_opname_pat_head(false) {
            self.open_dotted_opname_pat_head(kind);
            // The `constrPattern` tail, exactly as for an ident head: the explicit
            // value typars FCS takes after the whole `pathOp`, then the argument
            // group — the named-field form *or* the curried list, never both.
            if self.at_pat_typar_decls() {
                self.parse_typar_decls_postfix(true);
            }
            if self.at_name_pat_pairs() {
                self.parse_name_pat_pairs();
            } else {
                self.sweep_curried_arg_pats();
            }
            self.builder.finish_node(); // LONG_IDENT_PAT
            return true;
        }

        if !Self::is_atomic_pat_start(&self.filtered_tokens, self.pos) {
            return false;
        }

        // Function-form promotion: an ident head followed by an atomic-pat
        // start. This needs *both* a raw and a filtered lookahead, because
        // each rejects a case the other misses:
        //
        // - Raw (`next_non_trivia_raw_after`, past the head ident's byte
        //   end): a LexFilter-swallowed `)` is gone from the filtered
        //   stream, so a filtered-only peek would look *past* an enclosing
        //   paren's close and wrongly promote e.g. `(x) 0`'s `x`. The raw
        //   stream surfaces the swallowed `)` as `Token::RParen`, which
        //   `raw_starts_atomic_pat` rejects.
        // - Filtered (`next_non_trivia_filtered_after_pos`): layout
        //   virtuals (`Virtual::BlockSep`) live only in the filtered
        //   stream, so a raw-only peek skips straight over them. Inside an
        //   offside-laid-out list pattern `[ x⏎ y ]` the separator between
        //   `x` and `y` is exactly such a virtual; without this gate the
        //   raw peek sees `y` and promotes `x` to a zero-arg function-form
        //   head (the sweep then bails on the virtual), where FCS produces
        //   two distinct `Named` elements. Requiring the next filtered
        //   token itself be a `Raw` atomic-pat start rejects that.
        let head_ident_end = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), span)) => {
                Some(span.end)
            }
            _ => None,
        };
        // A *dotted* ident head (`Foo.Bar`) is a multi-segment long-ident path.
        // FCS always reduces it to `SynPat.LongIdent` (with whatever curried
        // args follow the *whole* path), so it takes the long-ident branch
        // below regardless of its trailing args — the arg sweep there handles
        // the nullary (`Foo.Bar`) and applied (`Foo.Bar x`) cases uniformly.
        // The single-ident function-form check keys on the *first* ident's end,
        // so it would miss args after a dotted path; routing dotted heads
        // straight to the long-ident branch sidesteps that.
        // A `global.`-rooted head (FCS's `GLOBAL DOT pathOp`) is a long-ident
        // path too, so it must take the long-ident branch below — otherwise a
        // curried arg after the path (`global.M.Case x`) is left unswept.
        // `head_ident_end` is `None` for the `global` keyword head (it isn't an
        // `Ident`), so it needs its own gate. A *bare* `global` (no dotted tail)
        // is not a rooted head: it falls to `emit_atomic_pat` (the bare-`global`
        // error). (The `_.`-rooted sibling is deferred behind its F# 4.7 gate.)
        //
        // A `global.`-rooted head routes through the same long-ident machinery as
        // any other dotted head, so it inherits that machinery's *pre-existing*,
        // not-`global`-specific limitation (which also rejects the ordinary
        // `A.B.…` form FCS accepts, and belongs to a separate follow-up): an
        // `opName` / active-pattern final segment (`global.N.(|Foo|_|)` /
        // `A.B.(|Foo|_|)`) — `sweep_long_ident_dot_continuation` accepts only
        // ident segments after a dot, so a `(`-led final segment errors.
        let rooted_global_head = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Global)), _))
        ) && self.pat_head_has_dotted_tail();
        let dotted_head =
            (head_ident_end.is_some() && self.pat_head_has_dotted_tail()) || rooted_global_head;
        // Explicit value-typar declarations on the head — `let f<'a> …`,
        // `let h<'a> = …` (FCS's `headBindingPattern … opt_explicitValTyparDecls`,
        // landing in `SynPat.LongIdent`'s `typars: SynValTyparDecls option` slot).
        // Carrying explicit typars *always* makes the head a `SynPat.LongIdent` —
        // even with zero curried args (`let h<'a> = 3` is a `LongIdent` with empty
        // `args`, not a `Named`) — so a *single-ident* head with typars must be
        // forced into the long-ident branch below alongside the function-form /
        // dotted cases. That is all this gate does; a dotted head already takes
        // that branch, and the typars themselves are recognised *after* the path
        // is swept ([`Self::at_pat_typar_decls`]), where FCS's
        // `constrPattern: atomicPatternLongIdent explicitValTyparDecls` puts them —
        // so `A.B.Case<'T>` and `global.M.Case<'T>` get them too.
        let head_typars_force_long_ident = head_ident_end.is_some()
            && !dotted_head
            && matches!(
                self.next_non_trivia_filtered_after_pos(),
                Some(
                    FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                        | FilteredToken::Raw(Token::Less(_))
                )
            );
        let is_function_form = !dotted_head
            && head_ident_end.is_some_and(|end| {
                let after = self.next_non_trivia_filtered_after_pos();
                // An atomic-pat-start *or* the `HighPrecedenceParenApp` virtual,
                // which LexFilter inserts before an *adjacent* paren arg (`f(x)`,
                // `f()`). The virtual is filtered-only, so the raw-after guard below
                // still sees the `(` it precedes (a paren-pattern start) and gates
                // promotion; a spaced `f (x)` has no virtual and matches the `Raw`
                // arm directly.
                let after_starts_pat = after.is_some_and(|ft| {
                    matches!(ft, FilteredToken::Raw(t) if raw_starts_atomic_pat(t))
                        || matches!(ft, FilteredToken::Virtual(Virtual::HighPrecedenceParenApp))
                });
                let raw_after = self.next_non_trivia_raw_after(end);
                // A sign-folded arg (`let f -1 = …`) surfaces the folded literal in
                // the *filtered* lookahead but the bare `Op("-")` sign in the *raw*
                // one (the fold rewrites only the filtered stream). Accept that:
                // raw-after is the fold's `±` sign AND the filtered-after is the
                // merged literal. Gating on the raw-after sign — not just the
                // filtered fold — preserves the swallowed-`)` reject: in
                // `let f (x) -1 = …` the inner `x`'s filtered-after sees `-1` past
                // the swallowed `)`, but its raw-after is `)`, not a sign, so `x`
                // stays a `Named` value rather than being promoted.
                let raw_after_is_fold_sign =
                    matches!(raw_after, Some(Token::Op(s)) if *s == "-" || *s == "+");
                let after_is_folded = after.is_some_and(
                    |ft| matches!(ft, FilteredToken::Raw(t) if token_is_folded_signed_literal(t)),
                );
                after_starts_pat
                    && (raw_after.is_some_and(raw_starts_atomic_pat)
                        || (raw_after_is_fold_sign && after_is_folded))
            });

        if !dotted_head && !is_function_form && !head_typars_force_long_ident {
            self.emit_atomic_pat();
        } else {
            // Function form (`f x`), a dotted long-ident head (`Foo.Bar`,
            // `Foo.Bar x`), or a generic head (`f<'a>`, `h<'a>`). Open
            // LONG_IDENT_PAT, emit the head path as a `LONG_IDENT` (the leading
            // ident plus any `.seg` continuation via
            // `sweep_long_ident_dot_continuation`), then the optional explicit
            // typar decls (`< … >`, between the head name and the args, mirroring
            // FCS's `SynPat.LongIdent` field order), then sweep up the curried
            // atomic-arg patterns. The sweep gate peeks the *raw* stream
            // (`next_non_trivia_raw_at_pos`): it stops at `,` (not an
            // atomic-pat start) so a function-form element followed by `, y`
            // falls into the surrounding tuple loop, and — crucially — it
            // stops at a LexFilter-swallowed `)` (surfaced as
            // `Token::RParen`) instead of folding the *next* curried paren
            // arg into this head, e.g. `f (Some x) (Some y)` keeps two
            // distinct paren args rather than `Some x (Some y)`.
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_PAT));
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
            self.bump_into(SyntaxKind::IDENT_TOK);
            self.sweep_long_ident_dot_continuation();
            self.builder.finish_node(); // LONG_IDENT
            if self.at_pat_typar_decls() {
                // `permit_empty = true`: a value binding's `explicitValTyparDeclsCore`
                // accepts an empty `let f< > x = x` (FCS: `Some(PostfixList [])`),
                // unlike a type definition's `postfixTyparDecls`.
                self.parse_typar_decls_postfix(true);
            }
            // FCS's `atomicPatsOrNamePatPairs` is *either* the named-field group
            // (`Case (field = pat; …)`, `SynArgPats.NamePatPairs`) *or* the
            // curried atomic-arg list (`SynArgPats.Pats`) — never both. The
            // named form is recognised by a `( ident =` lookahead.
            if self.at_name_pat_pairs() {
                self.parse_name_pat_pairs();
            } else {
                self.sweep_curried_arg_pats();
            }
            self.builder.finish_node(); // LONG_IDENT_PAT
        }
        true
    }

    /// Sweep the curried argument patterns of a function-form binding head — a
    /// `let f x y` / member `this.M x y` head — after the head ident
    /// (`LONG_IDENT`) has been emitted. Each arg is an atomic pattern; the
    /// raw-stream gate stops at `,` (a tuple boundary), a LexFilter-swallowed
    /// `)`, `=`, etc.
    ///
    /// An *adjacent* parenthesised arg (`f(x)`, `this.M(x)`, `f()`) is preceded
    /// by LexFilter's `HighPrecedenceParenApp` virtual (a spaced `f (x)` has
    /// none); the raw gate sees the `(` past it, so we consume the virtual
    /// zero-width before `try_emit_atomic_pat` so the paren pattern parses.
    ///
    /// The HPA is only valid (silent) when it applies to the head *name*
    /// (`f(x)`) or to a *paren* arg (the curried high-precedence chain
    /// `f(x)(y)` / `f (x)(y)`). After a non-paren atomic arg an adjacent paren
    /// is two successive patterns — FCS reports "Successive patterns should be
    /// separated by spaces or tupled" and recovers by parsing the paren anyway
    /// (`f x(y)` → `[Named x; Paren y]`, with an error). We mirror that:
    /// `prev_permits_hpa` starts `true` (the head name) and afterwards tracks
    /// whether the last arg was a paren; an HPA where it is `false` records the
    /// diagnostic but still parses the paren.
    pub(super) fn sweep_curried_arg_pats(&mut self) {
        let mut prev_permits_hpa = true;
        while self
            .next_non_trivia_raw_at_pos()
            .is_some_and(raw_starts_atomic_pat)
            || self.folded_signed_literal_at_cursor()
        {
            // Skip the `HighPrecedenceParenApp` virtual of an adjacent paren arg
            // so the cursor lands on the `(`.
            if let Some((Ok(FilteredToken::Virtual(Virtual::HighPrecedenceParenApp)), span)) =
                self.peek().cloned()
            {
                if !prev_permits_hpa {
                    self.errors.push(ParseError {
                        message: "Successive patterns should be separated by spaces or tupled"
                            .to_string(),
                        span,
                    });
                }
                self.bump_into(SyntaxKind::ERROR);
            }
            // An adjacent-paren arg always lands on `(` here (the HPA precedes
            // a `(`); a spaced arg is a paren iff the cursor is at `(`. A paren
            // arg permits a following adjacent paren (the curried chain).
            let arg_is_paren = matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::LParen)), _))
            );
            if !self.try_emit_atomic_pat() {
                break;
            }
            prev_permits_hpa = arg_is_paren;
        }
    }

    /// Emit an active-pattern name binding head when the cursor is at one,
    /// returning whether it fired. The `Named`-vs-`LongIdent` choice follows
    /// FCS's maybe-var collapse, exactly as for an ordinary single-segment ident
    /// head: the active pattern's `idText` leads with `|` (never an uppercase
    /// letter), so a *nullary* occurrence is var-like and collapses to
    /// `SynPat.Named` (`let (|Foo|Bar|) = …`, a `match`-clause head, a nested
    /// arg), while a *function-form* occurrence (curried args) or one carrying
    /// explicit value typars stays `SynPat.LongIdent` (`let (|Foo|Bar|) x = …`,
    /// `let (|Parse|_|)<'T> = …`). The `ACTIVE_PAT_NAME` node is the head
    /// segment in either wrapper.
    ///
    /// Called twice from [`Self::try_emit_head_binding_pat_element`]: once before
    /// the access-modifier scan (the unqualified head) and once after it (so
    /// `let private (|Foo|Bar|) …` is reached with the modifier already
    /// consumed).
    pub(super) fn try_emit_active_pat_head(&mut self) -> bool {
        if !self.at_active_pat_name() {
            return false;
        }
        let cp = self.builder.checkpoint();
        self.parse_active_pat_name();
        // Explicit value-typar decls after the name (`(|Parse|_|)<'T>`).
        // Active-pattern names are `atomicPatternLongIdent`s, so FCS accepts
        // typars and stores them on `SynPat.LongIdent.typars`. Carrying typars
        // forces the `LongIdent` form even with zero curried args (mirroring
        // `let h<'a> = …`).
        let has_typar_decls = self.at_pat_typar_decls();
        // Curried args follow iff the cursor is at an atomic-pat start. This
        // needs the *dual* lookahead the ident function-form promotion uses, for
        // the same reason: the raw stream alone skips a layout
        // `Virtual::BlockSep`, so a nullary name with an offside-separated
        // *sibling* element (`[ (|A|B|)⏎ y ]`) would wrongly promote to an
        // empty-arg `LONG_IDENT_PAT` and read `y` as its arg. Require the
        // *filtered* cursor itself to start an arg (a `Raw` atomic-pat start, an
        // adjacent-paren `HighPrecedenceParenApp` virtual, or a folded signed
        // literal), and the *raw* cursor too (to reject a swallowed `)`).
        // Active-pattern heads only ever carry curried args (FCS's
        // `SynArgPats.Pats`); the named-field form is union-case-only.
        let filtered_starts_arg = match self.peek() {
            Some((Ok(FilteredToken::Raw(t)), _)) => raw_starts_atomic_pat(t),
            Some((Ok(FilteredToken::Virtual(Virtual::HighPrecedenceParenApp)), _)) => true,
            _ => false,
        };
        let has_args = (filtered_starts_arg
            && self
                .next_non_trivia_raw_at_pos()
                .is_some_and(raw_starts_atomic_pat))
            || self.folded_signed_literal_at_cursor();
        if has_typar_decls || has_args {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_PAT));
            if has_typar_decls {
                // `permit_empty = true`: a value binding's
                // `explicitValTyparDeclsCore` accepts an empty `< >`.
                self.parse_typar_decls_postfix(true);
            }
            self.sweep_curried_arg_pats();
        } else {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::NAMED_PAT));
        }
        self.builder.finish_node();
        true
    }

    /// `true` when the cursor sits at the start of an active-pattern name —
    /// `(|Foo|Bar|)`, `(|Foo|_|)`. Detected by a `(` whose immediately
    /// following *raw* token is a bare `|` ([`Token::Bar`]). The raw lookahead
    /// (not the filtered one) is authoritative: it rejects the unit literal
    /// `()` (whose raw-after-`(` is `)`, a [`Token::RParen`]) and the
    /// pipe-operator value `(|>)` / `(||)` (whose `|` is glued into a
    /// [`Token::Op`] / [`Token::BarBar`], not a bare `Bar`), neither of which a
    /// filtered-only peek across the swallowed `)` would exclude. A bare `|`
    /// right after `(` is, in pattern position, unambiguously the leading bar
    /// of an active-pattern name (FCS's `LPAREN BAR …` active-pattern path).
    pub(super) fn at_active_pat_name(&self) -> bool {
        self.at_active_pat_name_at(self.pos)
    }

    /// As [`Self::at_active_pat_name`], but for the filtered token at `idx`
    /// rather than the cursor. Used by the long-ident qualification loop, where
    /// the segment's `(` sits one token past the `.` separator
    /// (`Foo.(|Bar|_|)`), so the active-pattern lookahead must be index-based
    /// (mirroring [`Self::at_paren_op_value`]'s `lparen_pos` parameter).
    pub(super) fn at_active_pat_name_at(&self, idx: usize) -> bool {
        let Some((Ok(FilteredToken::Raw(Token::LParen)), span)) = self.filtered_tokens.get(idx)
        else {
            return false;
        };
        matches!(self.next_non_trivia_raw_after(span.end), Some(Token::Bar))
    }

    /// `true` when the raw stream at/after `byte` opens an active-pattern name —
    /// its first two significant raw tokens are `(` then `|`. Used by the
    /// access-modifier gate to admit `let private (|Foo|Bar|) …` (FCS's `access
    /// pathOp`, where `pathOp` includes active-pattern operator names) while
    /// still rejecting a modifier before an ordinary paren pattern (`let private
    /// (x)`, whose second token is not `|`). Mirrors the raw scan in
    /// [`Self::next_non_trivia_raw_after`], collecting two tokens; a lex error
    /// stops it (returns `false`).
    pub(super) fn raw_active_pat_name_starts_after(&self, byte: usize) -> bool {
        // Binary-search to the first raw token at-or-after `byte` (spans are
        // sorted/contiguous) rather than rescanning from index 0 — see
        // [`Self::next_non_trivia_raw_after`].
        let start = self
            .raw_tokens
            .partition_point(|(_, span)| span.start < byte);
        let mut first: Option<&Token<'src>> = None;
        for (res, _) in &self.raw_tokens[start..] {
            match res {
                Ok(tt) => {
                    if let Some(t) = raw_significant(tt) {
                        match first {
                            None => first = Some(t),
                            Some(f) => {
                                return matches!(f, Token::LParen) && matches!(t, Token::Bar);
                            }
                        }
                    }
                }
                // A lex error stops the scan (mirrors `next_non_trivia_raw_after`).
                Err(_) => return false,
            }
        }
        false
    }

    /// Emit the [`SyntaxKind::ACTIVE_PAT_NAME`] node for an active-pattern name
    /// the cursor is positioned at (caller has verified [`Self::at_active_pat_name`]).
    /// Consumes `( | case | case | … |` from the filtered stream and the
    /// LexFilter-swallowed closing `)` from the raw stream, keeping every token
    /// for losslessness. Each case is a plain ident (`Foo`) or the partial-pattern
    /// `_`; the normaliser rebuilds FCS's single `idText` (`|Foo|Bar|`,
    /// `|Foo|_|`) from the case tokens.
    pub(super) fn parse_active_pat_name(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ACTIVE_PAT_NAME));
        // The `(` span anchors the malformed-name diagnostic below.
        let open_span = self.peek().map(|(_, span)| span.clone());
        self.bump_into(SyntaxKind::LPAREN_TOK); // `(`
        // Leading `|`. Guaranteed by `at_active_pat_name`, but guard so a
        // malformed stream mislabels nothing.
        let mut leading_bar = false;
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Bar)), _))) {
            self.bump_into(SyntaxKind::BAR_TOK);
            leading_bar = true;
        }
        // `(case `|`)+` — each case is an ident / `_`, followed by a `|`. The
        // closing `)` is LexFilter-swallowed, so it is absent from the filtered
        // stream: after the final `|` the filtered cursor already sits on
        // whatever follows the construct (a curried arg, `=`, `->`, …). Detect
        // the close on the *raw* stream so a following ident arg
        // (`let (|Foo|Bar|) x = …`) isn't mistaken for another case.
        //
        // Track the shape for the completeness check: a well-formed name has at
        // least one *ident* case, a trailing `|` immediately before the `)`, and
        // — for a partial name — the lone `_` marker only as the *final* case
        // (FCS's grammar puts `UNDERSCORE` only in `… BAR UNDERSCORE BAR rparen`,
        // so no case may follow a `_`, and there is at most one).
        let mut ident_cases = 0usize;
        let mut trailing_bar = false;
        let mut saw_underscore = false;
        let mut misplaced_underscore = false;
        loop {
            if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::RParen)) {
                break;
            }
            match self.peek() {
                Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), span)) => {
                    // FCS's `activePatternCaseName` action (`pars.fsy:6907`)
                    // validates each case ident and reports — recoverably, at the
                    // IDENT's range — `parsActivePatternCaseMustBeginWithUpperCase`
                    // (FS0623) when the leading char isn't upper-case, and
                    // `parsActivePatternCaseContainsPipe` (FS0624) when the
                    // `idText` holds a `|`. We mirror both here; the name node is
                    // still built either way, exactly as FCS keeps `$1`.
                    let span = span.clone();
                    let text = &self.source[span.clone()];
                    // `ident_text_leads_uppercase` strips the surrounding
                    // backticks itself, recovering FCS's `idText`. For the pipe
                    // check the raw lexeme suffices: the `` `` `` wrappers contain
                    // no `|`, so a backticked `idText` holds one iff the lexeme
                    // does (and a bare ident can never contain `|`).
                    let leads_upper = ident_text_leads_uppercase(text);
                    let contains_pipe = text.contains('|');
                    if !leads_upper {
                        self.errors.push(ParseError {
                            message: "active-pattern case identifier must begin \
                                      with an uppercase letter"
                                .to_string(),
                            span: span.clone(),
                        });
                    }
                    if contains_pipe {
                        self.errors.push(ParseError {
                            message: "the `|` character is not permitted in \
                                      active-pattern case identifiers"
                                .to_string(),
                            span,
                        });
                    }
                    self.bump_into(SyntaxKind::IDENT_TOK);
                    ident_cases += 1;
                    // A case after a `_` — the `_` was not final (`(|_|Foo|)`,
                    // `(|Foo|_|Bar|)`).
                    misplaced_underscore |= saw_underscore;
                }
                Some((Ok(FilteredToken::Raw(Token::Underscore)), _)) => {
                    self.bump_into(SyntaxKind::UNDERSCORE_TOK);
                    // A second `_`, or one following another case, is also a
                    // non-final marker (`(|_|_|)`).
                    misplaced_underscore |= saw_underscore;
                    saw_underscore = true;
                }
                _ => break,
            }
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Bar)), _))) {
                self.bump_into(SyntaxKind::BAR_TOK);
                trailing_bar = true;
            } else {
                trailing_bar = false;
                break;
            }
        }
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK); // `)`
        self.builder.finish_node(); // ACTIVE_PAT_NAME

        // A complete active-pattern name is `( | Case (| Case)* (| _)? | )` — a
        // leading `|`, at least one ident case, a trailing `|` before the `)`,
        // and a `_` (if any) only as the final case. FCS rejects every other
        // *structural* shape (`(|)` / `(|_|)` "Expected id", `(|Foo)`
        // "Expected '|'", `(|_|Foo|)` / `(|Foo|_|Bar|)` misplaced marker); we
        // recover losslessly (the node is kept) but report the error so they are
        // not silently accepted as a valid name — in expression position they
        // would otherwise parse as a clean `LONG_IDENT_EXPR`. The node still
        // round-trips, so callers (binders / the long-ident fold) need no extra
        // handling.
        //
        // The *per-case* naming rules — FS0623 (must begin upper-case) and
        // FS0624 (no `|` in the `idText`) — are enforced in the case loop above,
        // using `ident_text_leads_uppercase` (the BMP-exact
        // `String.isLeadingIdentifierCharacterUpperCase` replica that also drives
        // the `SynPat.LongIdent` vs `SynPat.Named` split).
        let complete = leading_bar && ident_cases >= 1 && trailing_bar && !misplaced_underscore;
        if let Some(span) = open_span
            && !complete
        {
            self.errors.push(ParseError {
                message: "incomplete active-pattern name; expected `(|Case|…|)`".to_string(),
                span,
            });
        }
    }

    /// `true` when the argument group of a function-form long-ident pattern is
    /// the named-field form (`Case (field = pat; …)`, FCS's
    /// `atomicPatsOrNamePatPairs → LPAREN namePatPairs rparen`,
    /// `SynArgPats.NamePatPairs`) rather than the curried atomic-arg list
    /// (`SynArgPats.Pats`). Pure lookahead, no consumption.
    ///
    /// Discriminated on a `( ident =` lookahead, with the *filtered* cursor
    /// first required to sit at a real `(` ([`Token::LParen`]) or the adjacent-
    /// paren [`Virtual::HighPrecedenceParenApp`] marker — never a layout virtual.
    /// The raw lookahead transparently skips the HPA virtual (it lives only in
    /// the filtered stream), so the adjacent form `Case(field = pat)` and the
    /// spaced form `Case (field = pat)` are detected identically; but it would
    /// *also* skip an offside-break layout virtual, so the filtered guard is what
    /// stops a dotted head reached past an offside break from mis-bumping that
    /// virtual as the `(`. This is the same filtered/raw discipline the curried
    /// path's `is_function_form` promotion uses. The `( ident =` prefix is
    /// unambiguous: `=` is not a pattern infix operator, so no `SynArgPats.Pats`
    /// parenthesised argument can begin that way (`(x)`, `(x : t)`, `(x, y)`,
    /// `()` all differ at the third token).
    pub(super) fn at_name_pat_pairs(&self) -> bool {
        let filtered_at_paren = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LParen)), _))
                | Some((
                    Ok(FilteredToken::Virtual(Virtual::HighPrecedenceParenApp)),
                    _
                ))
        );
        filtered_at_paren
            && matches!(self.nth_significant_raw_at_pos(0), Some(Token::LParen))
            && matches!(
                self.nth_significant_raw_at_pos(1),
                Some(Token::Ident(_) | Token::QuotedIdent(_))
            )
            && matches!(self.nth_significant_raw_at_pos(2), Some(Token::Equals))
    }

    /// Parse the named-field argument group `( field = pat ; … )` of a
    /// function-form long-ident pattern — FCS's `atomicPatsOrNamePatPairs →
    /// LPAREN namePatPairs rparen` (`pars.fsy:3750`), `SynArgPats.NamePatPairs`.
    /// Emits a [`SyntaxKind::NAME_PAT_PAIRS`] holding the parens and the
    /// `;`/`OBLOCKSEP`-separated [`SyntaxKind::NAME_PAT_PAIR`] fields. Caller
    /// (the function-form branch of [`Self::try_emit_head_binding_pat_element`])
    /// has verified [`Self::at_name_pat_pairs`].
    ///
    /// Modelled on [`Self::parse_record_pat`]: bump the `(`, parse one field,
    /// then loop one `seps_block` group per gap (so a repeated separator trips
    /// the field parser's recovery, matching FCS) until the swallowed `)`.
    /// Unlike the record form there is no empty-group production — the caller's
    /// `( ident =` lookahead guarantees the first field is present.
    pub(super) fn parse_name_pat_pairs(&mut self) {
        // An *adjacent* group (`Case(field = pat)`) is preceded by LexFilter's
        // `HighPrecedenceParenApp` virtual; consume it as a zero-width `ERROR`
        // before the `(`, exactly as `sweep_curried_arg_pats` does for an
        // adjacent paren arg. A spaced group has no virtual.
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceParenApp)),
                _
            ))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }

        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::NAME_PAT_PAIRS));
        self.bump_into(SyntaxKind::LPAREN_TOK);

        // The close `)` is LexFilter-swallowed, so probe the raw stream.
        let at_close =
            |p: &Self| matches!(p.next_non_trivia_raw_at_pos(), Some(Token::RParen) | None);

        if !at_close(self) {
            self.parse_name_pat_pair();
            // Subsequent fields after one `seps_block` group, mirroring
            // `parse_record_pat`: one group per gap, a trailing group before `)`
            // tolerated (`opt_seps_block`). `at_close` probes the raw stream so a
            // separator belonging to an *enclosing* scope (past the swallowed
            // `)`) is not drained as an inner one.
            while !at_close(self) && self.consume_one_seps_group(at_close) {
                if at_close(self) {
                    break;
                }
                self.parse_name_pat_pair();
            }
        }

        self.bump_swallowed_closer(
            SyntaxKind::RPAREN_TOK,
            |t| matches!(t, Token::RParen),
            ")",
            "named-field pattern",
        );
        self.builder.finish_node(); // NAME_PAT_PAIRS
    }

    /// One `NAME_PAT_PAIR > [IDENT_TOK (field name), EQUALS_TOK, <value
    /// parenPattern>]` — FCS's `namePatPair: ident EQUALS parenPattern`
    /// (`pars.fsy:3676`). The field name is a single `ident` (not a `path`, so
    /// `Case (M.X = p)` would be an FCS parse error); the value is a full
    /// in-delimiter `parenPattern` ([`Self::emit_paren_pat_element`] +
    /// [`Self::wrap_pat_tail`]`(cp, Paren)`), the same pair every other
    /// in-delimiter pattern element uses (and the same the record-field value
    /// runs).
    pub(super) fn parse_name_pat_pair(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::NAME_PAT_PAIR));

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
                message: "expected field name in named-field pattern".to_string(),
                span,
            });
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
                message: "expected `=` after named-field pattern field name".to_string(),
                span,
            });
        }

        let cp = self.builder.checkpoint();
        if self.emit_paren_pat_element() {
            self.wrap_pat_tail(cp, PatCtx::Paren);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected pattern after `=` in named-field pattern".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // NAME_PAT_PAIR
    }

    /// Binding power of `as` — loosest pattern infix (FCS `pars.fsy:248`).
    pub(super) const PAT_BP_AS: u8 = 10;

    /// Binding power of `|` (or-pattern), left-assoc — FCS `pars.fsy:266`,
    /// `%left BAR`. Looser than everything but `as`.
    pub(super) const PAT_BP_BAR: u8 = 20;

    /// Binding power of `,` (n-ary tuple) — FCS `pars.fsy:346`.
    pub(super) const PAT_BP_COMMA: u8 = 30;

    /// Binding power of the paren-only `: type` annotation — FCS `pars.fsy:350`.
    pub(super) const PAT_BP_COLON: u8 = 35;

    /// Binding power of `&` (n-ary conjunction) — FCS `pars.fsy:355`, `%left AMP`.
    pub(super) const PAT_BP_AMP: u8 = 40;

    /// Binding power of `::` — tightest pattern infix, right-assoc
    /// (FCS `pars.fsy:361`, `%right COLON_COLON`).
    pub(super) const PAT_BP_CONS: u8 = 50;

    /// Wrap the pattern checkpointed by `cp` in the trailing infix pattern
    /// operators that follow it — `::` (cons), `,` (tuple), `as`, and (in
    /// parens) `: type` — by **precedence climbing** over FCS's ambiguous,
    /// precedence-resolved pattern grammar.
    ///
    /// The first element has already been emitted by the caller; `cp` sits
    /// immediately before it. This is the `min_bp = 0` entry to
    /// [`Self::climb_pat_tail`]; see it for the per-operator semantics.
    ///
    /// Before phase 6.7 this was a token-order re-wrap loop, correct only for
    /// `as`/`,`/`:` (where token order happens to line up with the ladder).
    /// `::` is the *tightest* infix and right-associative, so it must bind
    /// inside tuple/`as` operands — token order no longer matches the tree, and
    /// the climber is required (e.g. `a, b :: c` ⇒ `Tuple[a, ListCons(b,c)]`,
    /// not `ListCons(Tuple[a,b], c)`).
    pub(super) fn wrap_pat_tail(&mut self, cp: rowan::Checkpoint, ctx: PatCtx) {
        self.climb_pat_tail(cp, 0, ctx);
    }

    /// Precedence-climbing pattern tail. `cp` marks the start of the
    /// already-emitted left operand; `min_bp` is the lowest operator binding
    /// power this call will consume (a parent climb raises it to stop the child
    /// at looser operators). Each operator (re-)wraps the *same* `cp` via
    /// `start_node_at`, so the accumulated left operand becomes the operator
    /// node's first child — left-to-right consumption maps to precedence-correct
    /// nesting.
    ///
    /// Binding powers mirror FCS's yacc ladder (`pars.fsy:244-374`); only the
    /// *ordering* matters. Loosest→tightest: `as`(248) < `|`(266) < `,`(346) <
    /// `: t`(350) < `&`(355) < `::`(361).
    ///
    /// Per-operator handling (all gated on `bp >= min_bp`; all peek the *raw*
    /// stream so a LexFilter-swallowed `)` doesn't trigger a spurious wrap):
    ///
    /// - **`::`** (`PAT_BP_CONS`, right-assoc) — `pars.fsy:3944`,
    ///   `%right COLON_COLON`. Wrap in [`SyntaxKind::LIST_CONS_PAT`]; the rhs is
    ///   a fresh element climbed at the *same* bp, so a following `::` nests
    ///   right (`a :: b :: c` ⇒ `ListCons(a, ListCons(b,c))`) while looser
    ///   operators stop it (`a :: b, c` ⇒ `Tuple[ListCons(a,b), c]`).
    /// - **`,`** (`PAT_BP_COMMA`, n-ary) — gather the whole comma-run into ONE
    ///   [`SyntaxKind::TUPLE_PAT`]. Each continuation element is climbed at
    ///   `PAT_BP_COMMA + 1`, so it captures tighter operators (`::`, the
    ///   per-element `:`) but stops at the next `,` and at looser `as`
    ///   (`a, b :: c` ⇒ `Tuple[a, ListCons(b,c)]`; `x, y as z` ⇒ the comma-run
    ///   reduces to `Tuple[x,y]` first, then the outer loop's `as` wraps it ⇒
    ///   `As(Tuple[x,y], z)`).
    /// - **`&`** (`PAT_BP_AMP`, n-ary) — `pars.fsy:3649`/`:4000`, `%left AMP`.
    ///   Gather the whole `&`-run into ONE flat [`SyntaxKind::ANDS_PAT`], the
    ///   same shape as the comma arm one rung tighter. Each operand climbs at
    ///   `PAT_BP_AMP + 1`, capturing `::` but stopping at the next `&` / looser
    ///   `,`/`as` (`a & b :: c` ⇒ `Ands[a, ListCons(b,c)]`; `a & b, c` ⇒
    ///   `Tuple[Ands[a,b], c]`).
    /// - **`|`** (`PAT_BP_BAR`, left-assoc) — `pars.fsy:3584`/`3916`,
    ///   `%left BAR`. The loosest infix but `as`. Wrap in
    ///   [`SyntaxKind::OR_PAT`]; the rhs climbs at `PAT_BP_BAR + 1`, so a
    ///   following `|` is caught by the outer loop (left-nested:
    ///   `A | B | C` ⇒ `Or(Or(A,B), C)`) while tighter operators bind into the
    ///   rhs (`A | B, C` ⇒ `Or(A, Tuple[B,C])`). In a `match` clause this fires
    ///   only in pattern position; a `|` after `-> result` is the clause
    ///   separator (`parse_match_clauses`), distinguished by the `->` boundary
    ///   at which the climber has already stopped.
    /// - **`as`** (`PAT_BP_AS`, loosest) — `pars.fsy:3570`/`3902`. The rhs is a
    ///   `constrPattern` (an applPat) via
    ///   [`Self::try_emit_head_binding_pat_element`], **not** climbed. That
    ///   atom-only rhs is what makes `a as b :: c` ⇒ `ListCons(As(a,b), c)`:
    ///   the `as` can't absorb the `::`, so it reduces first and the `::` wraps
    ///   the whole `As` (contrast `a :: b as c` ⇒ `As(ListCons(a,b), c)`).
    ///   Left-assoc `x as y as z` ⇒ `As(As(x,y), z)` falls out of re-wrapping
    ///   `cp`.
    /// - **`: type`** (`PAT_BP_COLON`, everything but [`PatCtx::Head`]) —
    ///   `pars.fsy:3929`. Wrap in [`SyntaxKind::TYPED_PAT`] and parse the
    ///   annotation with the *greedy* `typeWithTypeConstraints` (so it absorbs a
    ///   following `->` as a function type). In [`PatCtx::Paren`] a bare colon
    ///   survives to here only *after* an `as` (the per-element colon is consumed
    ///   inside [`Self::emit_paren_pat_element`]), so `(x as y : t)` ⇒
    ///   `Typed(As(x,y), t)`. In [`PatCtx::Clause`] it also fires for a bare
    ///   clause head `| pat: t …`: FCS admits an unparenthesised typed pattern
    ///   there, but because the type is greedy the construct only *parses* when
    ///   the annotation is bounded by `::`/`as` (`| h: int :: t ->`); a type
    ///   directly before the clause arrow (`| y: int -> e`) swallows the arrow
    ///   and both sides then error. [`PatCtx::Head`] excludes this arm: a
    ///   top-level `let pat : t = …` colon is the binding's `returnInfo`, not a
    ///   typed pattern.
    ///
    /// `ctx` ([`PatCtx`]) threads to every operand emit and recursive climb: it
    /// selects the `: type` arm (all but [`PatCtx::Head`]) and, in
    /// [`Self::emit_pat_atom`], which operands admit a leading `[<…>]` attribute
    /// prefix (`Paren`/`Clause`, never the bare `Head`).
    pub(super) fn climb_pat_tail(&mut self, cp: rowan::Checkpoint, min_bp: u8, ctx: PatCtx) {
        loop {
            match self.next_non_trivia_raw_at_pos() {
                Some(Token::ColonColon) if Self::PAT_BP_CONS >= min_bp => {
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::LIST_CONS_PAT));
                    self.bump_into(SyntaxKind::COLON_COLON_TOK);
                    // Right-associative: climb the rhs at the *same* bp so a
                    // following `::` nests into it but looser operators don't.
                    // Depth-guarded: this is the one deeply-recursive pattern
                    // climb (a long `a :: b :: c :: …` chain right-nests through
                    // here); the comma / `&` element climbs are sequential
                    // while-loop calls and `as` / `|` are left-associative, so
                    // only the cons recursion can grow the stack unboundedly.
                    let rhs_cp = self.builder.checkpoint();
                    if self.emit_pat_atom(ctx) {
                        self.with_depth(|p| p.climb_pat_tail(rhs_cp, Self::PAT_BP_CONS, ctx));
                    } else {
                        let span = self
                            .peek()
                            .map(|(_, s)| s.clone())
                            .unwrap_or_else(|| self.source.len()..self.source.len());
                        self.errors.push(ParseError {
                            message: "expected pattern after `::`".to_string(),
                            span,
                        });
                    }
                    self.builder.finish_node();
                }
                Some(Token::Comma) if Self::PAT_BP_COMMA >= min_bp => {
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TUPLE_PAT));
                    while self
                        .next_non_trivia_raw_at_pos()
                        .is_some_and(|t| matches!(t, Token::Comma))
                    {
                        self.bump_into(SyntaxKind::COMMA_TOK);
                        self.drain_block_sep_after_tuple_comma();
                        let el_cp = self.builder.checkpoint();
                        if self.emit_pat_atom(ctx) {
                            // Each element captures tighter ops (`::`, per-elem
                            // `:`) but stops at the next `,` / looser `as`.
                            self.climb_pat_tail(el_cp, Self::PAT_BP_COMMA + 1, ctx);
                        } else {
                            let span = self
                                .peek()
                                .map(|(_, s)| s.clone())
                                .unwrap_or_else(|| self.source.len()..self.source.len());
                            self.errors.push(ParseError {
                                message: "expected pattern after `,`".to_string(),
                                span,
                            });
                            break;
                        }
                    }
                    self.builder.finish_node();
                }
                Some(Token::Amp) if Self::PAT_BP_AMP >= min_bp => {
                    // N-ary conjunction, flat like the comma-tuple run but one
                    // rung tighter. `&` binds tighter than `,`/`:`/`as`, looser
                    // than `::`, so each operand climbs at `PAT_BP_AMP + 1`:
                    // captures `::` (`a & b :: c` ⇒ `Ands[a, ListCons(b,c)]`)
                    // but stops at the next `&` and at looser `,`/`as`
                    // (`a & b, c` ⇒ `Tuple[Ands[a,b], c]`; `a & b as c` ⇒
                    // `As(Ands[a,b], c)`). The first operand is already at `cp`.
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::ANDS_PAT));
                    while self
                        .next_non_trivia_raw_at_pos()
                        .is_some_and(|t| matches!(t, Token::Amp))
                    {
                        self.bump_into(SyntaxKind::AMP_TOK);
                        // Reuse the comma run's `Virtual::BlockSep` drain for a
                        // multi-line `a &\n b` continuation.
                        self.drain_block_sep_after_tuple_comma();
                        let el_cp = self.builder.checkpoint();
                        if self.emit_pat_atom(ctx) {
                            self.climb_pat_tail(el_cp, Self::PAT_BP_AMP + 1, ctx);
                        } else {
                            let span = self
                                .peek()
                                .map(|(_, s)| s.clone())
                                .unwrap_or_else(|| self.source.len()..self.source.len());
                            self.errors.push(ParseError {
                                message: "expected pattern after `&`".to_string(),
                                span,
                            });
                            break;
                        }
                    }
                    self.builder.finish_node();
                }
                Some(Token::As) if Self::PAT_BP_AS >= min_bp => {
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::AS_PAT));
                    self.bump_into(SyntaxKind::AS_TOK);
                    // The `as` rhs is `constrPattern` (an applPat) — never
                    // climbed — so a following `::`/`,`/`:` binds *outside* the
                    // `As` rather than into its rhs.
                    //
                    // Gate the rhs emit on the *raw* next token (like
                    // `emit_pat_atom` does for the `::`/`,` rhs) to respect the
                    // swallowed-`)` invariant: a LexFilter-swallowed `)` is gone
                    // from the filtered stream, so `try_emit_head_binding_pat_element`
                    // would otherwise look *past* it and consume a token
                    // belonging to the enclosing construct (e.g. `next` in
                    // `(h as) next`). The raw stream still surfaces the `)`, so
                    // the rhs bails cleanly — the `AS_PAT` gets no rhs, the `)`
                    // closes the paren, and `next` survives.
                    if !(self
                        .next_non_trivia_raw_at_pos()
                        .is_some_and(raw_starts_pat_element)
                        || self.folded_signed_literal_at_cursor())
                        || !self.try_emit_head_binding_pat_element()
                    {
                        let span = self
                            .peek()
                            .map(|(_, s)| s.clone())
                            .unwrap_or_else(|| self.source.len()..self.source.len());
                        self.errors.push(ParseError {
                            message: "expected pattern after `as`".to_string(),
                            span,
                        });
                        self.builder.finish_node();
                        break;
                    }
                    self.builder.finish_node();
                }
                Some(Token::Bar) if Self::PAT_BP_BAR >= min_bp => {
                    // Or-pattern — the loosest infix (only `as` is looser),
                    // left-associative and binary. The rhs climbs at
                    // `PAT_BP_BAR + 1`, so a following `|` is *not* absorbed into
                    // the rhs but caught by the outer loop re-wrapping `cp`
                    // (`A | B | C` ⇒ `Or(Or(A,B), C)`), while everything tighter
                    // (`::`/`&`/`,`/per-element `:`) binds into the rhs
                    // (`A | B, C` ⇒ `Or(A, Tuple[B,C])`).
                    //
                    // In a `match` clause this fires only in *pattern* position
                    // (before `->`); a `|` after `-> result` is the clause
                    // separator, owned by `parse_match_clauses` — the climber
                    // has already stopped at the `->` by then. The rhs emit is
                    // raw-gated (via `emit_pat_atom`) for the swallowed-`)`
                    // invariant, so `(A |) next` bails cleanly.
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::OR_PAT));
                    self.bump_into(SyntaxKind::BAR_TOK);
                    let rhs_cp = self.builder.checkpoint();
                    if self.emit_pat_atom(ctx) {
                        self.climb_pat_tail(rhs_cp, Self::PAT_BP_BAR + 1, ctx);
                    } else {
                        let span = self
                            .peek()
                            .map(|(_, s)| s.clone())
                            .unwrap_or_else(|| self.source.len()..self.source.len());
                        self.errors.push(ParseError {
                            message: "expected pattern after `|`".to_string(),
                            span,
                        });
                    }
                    self.builder.finish_node();
                }
                Some(Token::Colon) if ctx != PatCtx::Head && Self::PAT_BP_COLON >= min_bp => {
                    self.builder
                        .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TYPED_PAT));
                    self.bump_into(SyntaxKind::COLON_TOK);
                    self.parse_type_with_constraints();
                    self.builder.finish_node();
                }
                _ => break,
            }
        }
    }

    /// The pattern-tail "atom" (a `constrPattern` element) the climber consumes
    /// for a `::` rhs or a `,` / `&` / `|` continuation element, choosing the
    /// per-element form by `ctx`: [`PatCtx::Paren`] →
    /// [`Self::emit_paren_pat_element`] (an applPat plus an optional per-element
    /// `: type`); [`PatCtx::Head`] / [`PatCtx::Clause`] →
    /// [`Self::try_emit_head_binding_pat_element`] (applPat only — a top-level
    /// `:` is `SynBinding.returnInfo`; a clause `:` *is* a valid typed pattern,
    /// but it is consumed by the [`Self::climb_pat_tail`] colon arm, not here).
    /// Returns `false` (without consuming) when no element follows.
    ///
    /// A `parenPattern` operand ([`PatCtx::Paren`] or [`PatCtx::Clause`], never
    /// the bare [`PatCtx::Head`]) also admits a leading `[<…>]` attribute prefix
    /// → `SynPat.Attrib` (phase 10.6), dispatched to [`Self::emit_attrib_pat`];
    /// so `(y, [<A>] x)` and `match v with A | [<B>] x -> _` both attach the
    /// attribute to the tail operand. The bare head rejects `[<` (FCS's
    /// `headBindingPattern` has no attributes production, so `let x, [<A>] y` is
    /// a parse error).
    ///
    /// The non-attribute emit is gated on the *raw* next token being a
    /// [`raw_starts_pat_element`] start, **not** the filtered cursor, to respect
    /// the swallowed-`)` invariant: a LexFilter-swallowed `)` is gone from the
    /// filtered stream, so the underlying parsers would otherwise look *past* it
    /// and consume a token belonging to an enclosing construct (e.g. the `next`
    /// in `(h ::) next`). The raw stream still surfaces the `)`, so the gate
    /// rejects it and the operator's rhs bails cleanly. (A layout virtual at the
    /// filtered cursor is rejected too — the filtered parsers see it as a
    /// non-atomic-start — so offside breaks past the operator also recover.)
    /// `emit_attrib_pat` applies the same raw guard to its own inner pattern.
    pub(super) fn emit_pat_atom(&mut self, ctx: PatCtx) -> bool {
        // Attribute-prefixed operand — valid at every `parenPattern` operand,
        // never at the bare head. Dispatched here (not through the
        // `emit_paren_pat_element` / `try_emit_head_binding_pat_element` split)
        // since the clause path uses the head-element parser, which has no
        // attrib hook. `at_attribute_list_start` requires a real `[<` on both
        // cursors, so an offside continuation or a swallowed closer before the
        // `[<` declines here and falls through to the raw-gated path below.
        if ctx != PatCtx::Head && self.at_attribute_list_start() {
            let cp = self.builder.checkpoint();
            self.emit_attrib_pat(cp, ctx);
            return true;
        }
        // A sign-folded literal (`1, -1` / `1 :: -1`) is the parser's next
        // filtered token but shows a bare `Op("-")` in the raw lookahead, so
        // accept it explicitly; `folded_signed_literal_at_cursor` keeps the
        // swallowed-`)` guard (it requires the raw cursor to begin at the
        // literal, not at an earlier closer).
        let raw = self.next_non_trivia_raw_at_pos();
        // A per-element access modifier (`a, private b`, `a :: internal b`,
        // `(a, private b)`) — FCS's `access pathOp` before a tuple/cons/clause/
        // paren element, not just the head. It is not a pattern *start* (so it is
        // absent from `raw_starts_pat_element`), but every element emitter homes on
        // [`Self::try_emit_head_binding_pat_element`] (the `Paren` one via
        // [`Self::emit_paren_pat_element`]), which consumes it before an ident /
        // active-pattern / operator `pathOp`; admit it here so that emitter is
        // reached. A modifier before a non-`pathOp` (`private _`, `private 1`) is
        // not consumed there and the element still bails — an FCS error on both
        // sides.
        let access_before_element =
            matches!(raw, Some(Token::Private | Token::Internal | Token::Public));
        if !raw.is_some_and(raw_starts_pat_element)
            && !self.folded_signed_literal_at_cursor()
            && !access_before_element
        {
            return false;
        }
        match ctx {
            PatCtx::Paren => self.emit_paren_pat_element(),
            PatCtx::Head | PatCtx::Clause => self.try_emit_head_binding_pat_element(),
        }
    }

    /// Multi-line tuple patterns like `(x,\n    y)` see LexFilter emit a
    /// `Virtual::BlockSep` between the comma and the next element — the
    /// same offside scaffolding that the expression-side tuple loop
    /// already drains (see `parse_expr` ~1175). Stamp each one as
    /// `ERROR` so it survives in the green tree without affecting the
    /// typed-AST projection, mirroring the expression site's discipline.
    pub(super) fn drain_block_sep_after_tuple_comma(&mut self) {
        while matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
    }

    /// `true` when the parser is positioned at a real `[<` attribute-list
    /// opener — `Token::LBrackLess` on **both** the filtered cursor (`peek`)
    /// *and* the raw lookahead (`next_non_trivia_raw_at_pos`). The two cursors
    /// disagree exactly at the recovery hazards [`Self::emit_attrib_pat`]'s
    /// opener `bump_into` can't see, so every `[<`-dispatch site
    /// ([`Self::emit_pat_atom`], [`Self::emit_paren_pat_element`], and the
    /// `match`-clause head) must consult this rather than a filtered-only peek:
    ///   * a layout virtual parked at the filtered cursor (an offside
    ///     continuation): raw skips it and sees `[<`, so a raw-only check would
    ///     bump the virtual *as* `[<`; the filtered check rejects it; and
    ///   * a LexFilter-swallowed closer (`)`/`}`) at the raw cursor with a
    ///     following `[<` outside (`( a :: ) [<A>] b`, `{ X = } [<A>] y`):
    ///     filtered skips the closer and sees the outside `[<`, so a
    ///     filtered-only check would drain the closer as `ERROR` and steal the
    ///     outside attribute; the raw check rejects it.
    ///
    /// Requiring both real `[<`s declines in either hazard, letting the closer
    /// be reclaimed / the offside break recover, exactly as the raw-gated
    /// non-attributed operand emits already do.
    pub(super) fn at_attribute_list_start(&self) -> bool {
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) && matches!(self.next_non_trivia_raw_at_pos(), Some(Token::LBrackLess))
    }

    /// `true` when `filtered_tokens[pos]` is a token that can start an
    /// atomic pattern in head/arg position. Pure inspection; no
    /// consumption. Phase 6.1 covers:
    ///
    /// - `Token::Ident`/`Token::QuotedIdent` → [`SyntaxKind::NAMED_PAT`].
    /// - `Token::Underscore` → [`SyntaxKind::WILDCARD_PAT`].
    /// - `Token::Null` → [`SyntaxKind::NULL_PAT`].
    /// - `Token::QMark` (`?ident`) → [`SyntaxKind::OPTIONAL_VAL_PAT`].
    /// - `Token::LParen` → [`SyntaxKind::PAREN_PAT`] (or the unit-literal
    ///   form of [`SyntaxKind::CONST_PAT`] when the parens are empty).
    /// - The const-literal token set accepted by
    ///   [`raw_starts_const_payload`] → [`SyntaxKind::CONST_PAT`].
    /// - `Token::LQuote`/`Token::LQuoteRaw` (`<@`/`<@@`) → [`SyntaxKind::QUOTE_PAT`].
    pub(super) fn is_atomic_pat_start(filtered_tokens: &[FilteredTok<'src>], pos: usize) -> bool {
        matches!(
            filtered_tokens.get(pos),
            Some((Ok(FilteredToken::Raw(t)), _)) if raw_starts_atomic_pat(t)
        )
    }

    /// Emit the next atomic pattern and consume its underlying tokens.
    /// Caller must have verified [`Self::is_atomic_pat_start`] for the
    /// current position; otherwise this debug-asserts.
    pub(super) fn emit_atomic_pat(&mut self) {
        let ok = self.try_emit_atomic_pat();
        debug_assert!(
            ok || self.depth_limit_hit,
            "emit_atomic_pat called without an atomic pat start at pos {}",
            self.pos
        );
    }

    /// Emit an attributed pattern `[< … >] parenPattern` →
    /// `SynPat.Attrib(inner, attrs)` (`pars.fsy:3940`), wrapping everything
    /// from `cp` (which must sit *before* the attribute lists) in an
    /// `ATTRIB_PAT`. The caller has verified `peek()` is `Token::LBrackLess`
    /// and owns the *outer* tail (`,`/`as`/`|`) via its own `wrap_pat_tail` on
    /// the same `cp`; this consumes only the attribute lists, the inner
    /// pattern, and the operators that bind *inside* the attrib (`:`/`&`/`::`).
    ///
    /// `ctx` threads to the inner climb (its per-element `:` arm fires only for
    /// [`PatCtx::Paren`]), matching the caller's `wrap_pat_tail` convention —
    /// `Paren` inside parens / list / array elements, `Clause` at a
    /// `match`/`function` clause head.
    ///
    /// The inner emit is raw-gated (like [`Self::emit_pat_atom`]'s `::`/`,`
    /// rhs): a LexFilter-swallowed `)` is gone from the filtered stream, so
    /// without the guard `([<A>]) x` (attribute then an immediate `)`) would
    /// drain the `)` as `ERROR` and steal the following `x` as the inner
    /// pattern. The raw stream still surfaces the `)`, so the inner bails
    /// cleanly — the `ATTRIB_PAT` gets no inner pat, the `)` closes the
    /// delimiter, and `x` survives as the next element / argument.
    pub(super) fn emit_attrib_pat(&mut self, cp: rowan::Checkpoint, ctx: PatCtx) {
        self.parse_attribute_lists();
        // FCS's `attributeList` carries a trailing `opt_OBLOCKSEP`, so the
        // attributed pattern may sit on a fresh offside line (`([<A>]⏎  x)`).
        // LexFilter parks a `Virtual::BlockSep` at the filtered cursor there;
        // the raw lookahead skips it (so `inner_starts` already sees `x`) but
        // `try_emit_head_binding_pat_element` peeks the filtered cursor and
        // would otherwise stall on the virtual. Drain it as a zero-width `ERROR`
        // (same generic `Virtual::BlockSep` drain the tuple / `&` runs use).
        self.drain_block_sep_after_tuple_comma();
        // The attrib prefix binds tighter than `,`/`as`/`|` (the caller's tail
        // wraps the whole `ATTRIB_PAT`) and looser than `:`/`&`/`::` — so the
        // inner climbs at `PAT_BP_COLON`, absorbing `:`(35, when `ctx` is
        // `Paren`) / `&`(40) / `::`(50) and stopping at `,`(30) / `|`(20) /
        // `as`(10). Verified against FCS.
        let inner_cp = self.builder.checkpoint();
        let inner_starts = self
            .next_non_trivia_raw_at_pos()
            .is_some_and(raw_starts_pat_element)
            || self.folded_signed_literal_at_cursor();
        if inner_starts && self.try_emit_head_binding_pat_element() {
            self.climb_pat_tail(inner_cp, Self::PAT_BP_COLON, ctx);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected pattern after attribute list".to_string(),
                span,
            });
        }
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::ATTRIB_PAT));
        self.builder.finish_node();
    }

    /// Try to consume a parenthesised-pat element: an applPat (atomic
    /// or function-form `Ctor arg1 arg2`) optionally followed by
    /// `: <type>`, wrapping in [`SyntaxKind::TYPED_PAT`] when the colon
    /// is present. Used by [`Self::parse_paren_pat`] for both the
    /// single-element case and each element of a parenthesised tuple, and
    /// (via [`Self::emit_pat_atom`]) for list/array elements.
    ///
    /// Inside parens FCS's grammar attaches `: type` to the immediately
    /// preceding element, not to the surrounding tuple, so
    /// `(x, y : int)` projects to
    /// `Paren(Tuple([Named x, Typed(Named y, int)]))`. Each element is
    /// itself an applPat, so `(x, Some y)` projects to
    /// `Paren(Tuple([Named x, LongIdent("Some", [Named y])]))` —
    /// matching the top-level `maybe_wrap_tuple_pat` discipline.
    ///
    /// A leading `[< … >]` makes the element an attributed pattern
    /// (`SynPat.Attrib`, phase 10.6) via [`Self::emit_attrib_pat`].
    ///
    /// Top-level binding heads do NOT use this: at that position a
    /// trailing `:` is `SynBinding.returnInfo`, not a typed-pat.
    ///
    /// Returns `true` when an element was emitted (with or without the
    /// typed wrap); `false` (without consuming or emitting) when the
    /// cursor isn't at an atomic-pat start.
    pub(super) fn emit_paren_pat_element(&mut self) -> bool {
        let cp = self.builder.checkpoint();

        // Phase 10.6 — a leading `[< … >]` prefixes an attributed parenPattern
        // (`SynPat.Attrib`). Reachable at this in-delimiter level (parens /
        // list / array elements / record-field values) and at `match`/`function`
        // clause heads (see `parse_match_clauses`), but never at a bare binding
        // head — FCS's grammar puts `attributes` on `parenPattern`, not
        // `headBindingPattern`. `at_attribute_list_start` requires a real `[<`
        // on both cursors so a missing element before a swallowed closer
        // (`{ X = } [<A>] y`) declines here rather than draining the `}` and
        // stealing the outside attribute.
        if self.at_attribute_list_start() {
            self.emit_attrib_pat(cp, PatCtx::Paren);
            return true;
        }

        if !self.try_emit_head_binding_pat_element() {
            return false;
        }
        if self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Colon))
        {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TYPED_PAT));
            self.bump_into(SyntaxKind::COLON_TOK);
            self.parse_type_with_constraints();
            self.builder.finish_node();
        }
        true
    }

    /// True when the cursor sits at an identifier head immediately followed by
    /// a `DOT ident` continuation — i.e. the head is a *multi-segment*
    /// long-ident pattern path (`Foo.Bar`), not a single ident. FCS classifies
    /// any multi-segment `atomicPatternLongIdent` as `SynPat.LongIdent`
    /// regardless of the head's case (`pars.fsy:3810`,
    /// `not (isNilOrSingleton …)`), so this drives the `LONG_IDENT_PAT` vs
    /// `NAMED_PAT` choice for a lowercase head.
    ///
    /// Layout-safe and aligned with [`Self::sweep_long_ident_dot_continuation`]
    /// (which actually consumes the tail), so classification and emission
    /// agree: it requires the dot to be the *next filtered token* (so an
    /// offside `Foo⏎.Bar` is not a single path) and a real identifier to follow
    /// it on the raw stream (so a trailing `Foo.` stays single-segment, the dot
    /// becoming a separate error during the sweep).
    ///
    /// Head-token agnostic: it inspects only the tokens *after* the head, so it
    /// gates the `pathOp` alternative (an *ident*-rooted path) **and** FCS's
    /// `GLOBAL DOT pathOp` (`global.N.Case`). `global` lexes as [`Token::Global`];
    /// the dispatch arm in [`Self::try_emit_atomic_pat_inner`] (and the
    /// `rooted_global_head` gate in [`Self::try_emit_head_binding_pat_element`],
    /// for the applied `global.M.Case x` form) consults this to route the head to
    /// the same `LONG_IDENT_PAT` emission as an ident path. A *bare* `global` (no
    /// dotted tail) is an error (FCS FS0010), never a pattern name.
    ///
    /// FCS's sibling `UNDERSCORE DOT pathOp` (`_.M`) is **not** handled here: it
    /// is gated on the F# 4.7 `SingleUnderscorePattern` feature (`pars.fsy:2255`)
    /// and lands with that language-version gate in a later slice.
    pub(super) fn pat_head_has_dotted_tail(&self) -> bool {
        matches!(
            self.next_non_trivia_filtered_after_pos(),
            Some(FilteredToken::Raw(Token::Dot))
        ) && matches!(
            self.nth_significant_raw_at_pos(2),
            Some(Token::Ident(_) | Token::QuotedIdent(_))
        )
    }

    /// `Some(kind)` when the cursor is at a dotted pattern path whose *final*
    /// segment is an `opName` — an operator value (`A.B.(+)`, the spaced
    /// `A.B.( * )`, the glued `A.B.(*)`) or an active-pattern name
    /// (`A.B.(|Foo|_|)`) — and `None` otherwise (a plain dotted path `A.B.C`,
    /// whose post-`.` token is an ident, stays on the generic long-ident branch).
    ///
    /// This is FCS's `pathOp` (`pars.fsy:6930`): `ident (DOT ident)* [DOT opName]`.
    /// Only the *last* segment may be an `opName`, so the lookahead walks the
    /// intermediate `. ident` segments and inspects the token after the final `.`.
    /// Every `atomicPatternLongIdent` admits it — a `match`-clause head, a `let`
    /// binding head, a curried atomic argument, and the member self-id head
    /// (`member x.(+)`, whose `<self-id> .` is just this path's first segment).
    ///
    /// `allow_underscore_head` admits a `_` head segment: a *member* self-id may be
    /// `_` (`member _.(+) …`), whereas a pattern's `_.`-rooted path is FCS's
    /// `UNDERSCORE DOT pathOp`, gated on the F# 4.7 `SingleUnderscorePattern`
    /// feature (`pars.fsy:2255`) and deferred here with its sibling forms — see
    /// [`Self::pat_head_has_dotted_tail`]. Pure lookahead, no consumption.
    pub(super) fn peek_dotted_opname_pat_head(
        &self,
        allow_underscore_head: bool,
    ) -> Option<DottedOpNameHead> {
        let head_ok = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _)) => allow_underscore_head,
            Some((
                Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_) | Token::Global)),
                _,
            )) => true,
            _ => false,
        };
        if !head_ok {
            return None;
        }
        // Walk the dotted path `<head> (. ident)*` to the *final* `.`, whose
        // following token is the `opName`.
        let mut seg_idx = self.pos;
        loop {
            let dot_idx = self.next_non_trivia_filtered_index_after(seg_idx)?;
            if !matches!(
                self.filtered_tokens.get(dot_idx),
                Some((Ok(FilteredToken::Raw(Token::Dot)), _))
            ) {
                return None;
            }
            let after_idx = self.next_non_trivia_filtered_index_after(dot_idx)?;
            if let Some(kind) = self.opname_pat_kind_at(after_idx) {
                return Some(kind);
            }
            // Not an `opName`: only a further `. ident` continuation keeps the
            // dotted-path shape alive (the intermediate segments are plain
            // idents). Anything else (`A.B.C`, an ordinary dotted path) is not
            // this form.
            if !matches!(
                self.filtered_tokens.get(after_idx),
                Some((
                    Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                    _
                ))
            ) {
                return None;
            }
            seg_idx = after_idx;
        }
    }

    /// The `opName` kind at filtered index `idx` — an operator-value (`( op )`,
    /// glued `(*)`) or active-pattern (`(|Foo|Bar|)`) name — or `None`. Shared by
    /// [`Self::peek_dotted_opname_pat_head`]'s lookahead and
    /// [`Self::open_dotted_opname_pat_head`]'s segment walk.
    fn opname_pat_kind_at(&self, idx: usize) -> Option<DottedOpNameHead> {
        if matches!(
            self.filtered_tokens.get(idx),
            Some((Ok(FilteredToken::Raw(Token::LParenStarRParen)), _))
        ) {
            Some(DottedOpNameHead::Operator { is_star: true })
        } else if self.at_paren_op_value_pat(idx) {
            Some(DottedOpNameHead::Operator { is_star: false })
        } else if self.at_active_pat_name_at(idx) {
            Some(DottedOpNameHead::ActivePat)
        } else {
            None
        }
    }

    /// Emit the head of a dotted `opName` pattern path (detected by
    /// [`Self::peek_dotted_opname_pat_head`]): opens a
    /// [`SyntaxKind::LONG_IDENT_PAT`] and consumes `<head> (. ident)* . <opName>`.
    /// The `LONG_IDENT_PAT` is left **open** — the caller emits whatever tail its
    /// grammar position allows (typars / curried args, or nothing at all for an
    /// atomic-argument occurrence) and calls `finish_node`.
    ///
    /// The path segments and the final `.` head the `LONG_IDENT`; an operator
    /// name's `( op )` tokens are appended *inside* that `LONG_IDENT` (so its
    /// `idents()` reads `["A", "B", "+"]`, matching FCS's
    /// `["A"; "B"; "op_Addition"]`), while an active-pattern name is emitted as a
    /// sibling `ACTIVE_PAT_NAME` (the normaliser appends its folded `|Foo|Bar|`
    /// segment after the path, matching FCS's `["A"; "B"; "|Foo|Bar|"]`).
    pub(super) fn open_dotted_opname_pat_head(&mut self, kind: DottedOpNameHead) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_PAT));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        // The head segment (an ident, a member self-id `_`, or the `global` root
        // marker) — emitted as `IDENT_TOK` like any long-ident path segment.
        self.bump_into(SyntaxKind::IDENT_TOK);
        // Intermediate `. ident` segments of a multi-segment path (`A.B.(+)`):
        // bump each `. ident` whose post-`.` token is *not* the `opName`. The loop
        // stops at the final `.` (its successor is the `opName`), bumped just below.
        while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Dot)), _))) {
            let after_idx = match self.next_non_trivia_filtered_index_after(self.pos) {
                Some(idx) if self.opname_pat_kind_at(idx).is_none() => idx,
                _ => break,
            };
            debug_assert!(matches!(
                self.filtered_tokens.get(after_idx),
                Some((
                    Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                    _
                ))
            ));
            self.bump_into(SyntaxKind::DOT_TOK);
            self.bump_into(SyntaxKind::IDENT_TOK);
        }
        // The final `.` before the `opName`.
        self.bump_into(SyntaxKind::DOT_TOK);
        match kind {
            DottedOpNameHead::Operator { is_star } => {
                if is_star {
                    self.consume_star_op_value();
                } else {
                    self.consume_paren_op_value();
                }
                self.builder.finish_node(); // LONG_IDENT (path, dot, op tokens)
            }
            DottedOpNameHead::ActivePat => {
                self.builder.finish_node(); // LONG_IDENT (path, dot)
                self.parse_active_pat_name();
            }
        }
    }

    /// `true` when the cursor — positioned just past a pattern head's *name*
    /// (an ident path, an operator name, an active-pattern name) — sits at the
    /// explicit value-typar declarations FCS's `constrPattern:
    /// atomicPatternLongIdent explicitValTyparDecls …` (`pars.fsy:3689`) takes
    /// there. A `<` in this position can only open type parameters (pattern
    /// syntax has no infix `<`), so it is detected the same way the
    /// type-definition header does: the adjacent form (`Case<'T>`) is preceded by
    /// LexFilter's `HighPrecedenceTyApp` virtual, the spaced form (`Case <'T>`,
    /// which FCS accepts with a warning) shows a bare raw `Less`.
    ///
    /// Because FCS attaches the typars to the whole `pathOp`, this is checked
    /// *after* the head path has been swept — so a dotted (`A.B.Case<'T>`) or
    /// `global.`-rooted (`global.M.Case<'T>`) head carries typars exactly as a
    /// bare `Case<'T>` does.
    pub(super) fn at_pat_typar_decls(&self) -> bool {
        matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                    | FilteredToken::Raw(Token::Less(_))),
                _
            ))
        )
    }

    /// `Some(is_star)` when the cursor is at an operator-name binding head — the
    /// glued `(*)` (`is_star = true`, the dedicated [`Token::LParenStarRParen`])
    /// or a general / spaced `( op )` (`is_star = false`; includes the spaced
    /// `( * )` via [`Self::at_paren_op_value_pat`], which admits the star in
    /// pattern position). `None` otherwise. Pure lookahead.
    pub(super) fn peek_operator_head(&self) -> Option<bool> {
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LParenStarRParen)), _))
        ) {
            Some(true)
        } else if self.at_paren_op_value_pat(self.pos) {
            Some(false)
        } else {
            None
        }
    }

    /// The head's *filtered-after* and *raw-after* tokens — the lookaheads past
    /// the operator-name head's close, used to decide its typars / args. `is_star`
    /// selects the anchor: the glued `(*)` token is self-contained, so its
    /// lookaheads start past its own byte end; the general `( op )` ends in a
    /// LexFilter-swallowed `)`, so the raw lookahead steps past *that* close
    /// ([`Self::raw_after_paren_op_close`]) and the filtered lookahead past the
    /// operator token (the swallowed `)` is already absent from the filtered
    /// stream). Caller has verified [`Self::peek_operator_head`] `== Some(is_star)`.
    pub(super) fn operator_head_after(
        &self,
        is_star: bool,
    ) -> (Option<&FilteredToken<'src>>, Option<&Token<'src>>) {
        if is_star {
            let star_end = self.filtered_tokens.get(self.pos).map(|(_, s)| s.end);
            (
                self.next_non_trivia_filtered_after_pos(),
                star_end.and_then(|end| self.next_non_trivia_raw_after(end)),
            )
        } else {
            // The operator name may span two filtered tokens (the range-step
            // `.. ..`), so take its *last* index and end byte from the shared
            // recogniser rather than assuming the op is at `self.pos + 1`.
            let (last_index, op_end) = self
                .paren_op_name_end(self.pos, true)
                .expect("operator_head_after: caller verified a paren operator value");
            (
                self.next_non_trivia_filtered_after_index(last_index),
                self.raw_after_paren_op_close(op_end),
            )
        }
    }

    /// "Do curried args follow an operator-name head?", given the head's
    /// *filtered-after* (`after`) and *raw-after* (`raw_after`) tokens (from
    /// [`Self::operator_head_after`]). `true` contributes the applied
    /// `SynPat.LongIdent` form; `false` (with no typars) the nullary
    /// `SynPat.Named`.
    ///
    /// Mirrors the ident function-form promotion's dual raw+filtered discipline
    /// (each stream rejects a hazard the other misses):
    ///
    /// - **Filtered**: must start an atomic pattern or be the adjacent-paren
    ///   `HighPrecedenceParenApp` virtual — rejects an offside break
    ///   (`let (+)⏎  x = …`, a layout virtual the raw stream skips over).
    /// - **Raw**: must start an atomic pattern, *or* be a fold sign (`-`/`+`)
    ///   whose filtered form is the folded signed literal (`let (+) -1 = …`,
    ///   where `sign_fold` rewrites only the filtered stream). Reading the raw
    ///   stream also rejects a LexFilter-swallowed *enclosing* `)` (`let f
    ///   ((op)) x = …`, surfaced as `Token::RParen`) the filtered stream skips.
    pub(super) fn op_head_args_follow(
        after: Option<&FilteredToken<'src>>,
        raw_after: Option<&Token<'src>>,
    ) -> bool {
        let filtered = matches!(after, Some(FilteredToken::Raw(t)) if raw_starts_atomic_pat(t))
            || matches!(
                after,
                Some(FilteredToken::Virtual(Virtual::HighPrecedenceParenApp))
            );
        let raw = raw_after.is_some_and(raw_starts_atomic_pat)
            || (matches!(raw_after, Some(Token::Op(s)) if *s == "-" || *s == "+")
                && matches!(after, Some(FilteredToken::Raw(t)) if token_is_folded_signed_literal(t)));
        filtered && raw
    }

    /// Emit an operator-name binding head, the pattern analogue of the ident
    /// function-form / generic head below. `is_star` selects the glued `(*)`
    /// token ([`Self::consume_star_op_value`]) over the general / spaced `( op )`
    /// ([`Self::consume_paren_op_value`]) — both emit `[LPAREN_TOK,
    /// IDENT_TOK(op), RPAREN_TOK]` (the source operator spelling under
    /// `IDENT_TOK`, the differential normaliser de-quoting FCS's mangled
    /// `op_*` + `OriginalNotationWithParen` to match).
    ///
    /// * Nullary (no typars, no args) → `NAMED_PAT` — FCS's singleton-lowercase
    ///   `atomicPattern: atomicPatternLongIdent` reduction (`SynPat.Named`).
    /// * Typars and/or args → `LONG_IDENT_PAT > [LONG_IDENT, TYPAR_DECLS?,
    ///   <args…>]` — the `constrPattern` reduction (`SynPat.LongIdent`). The
    ///   typar decls (`let (!!)<'T> … `) and the `atomicPatsOrNamePatPairs` arg
    ///   group (curried `Pats` *or* named-field `NamePatPairs`, never both) reuse
    ///   the exact ident-head machinery, so the field order matches FCS.
    pub(super) fn emit_operator_head(&mut self, is_star: bool, has_typars: bool, has_args: bool) {
        let consume_op = |p: &mut Self| {
            if is_star {
                p.consume_star_op_value();
            } else {
                p.consume_paren_op_value();
            }
        };
        if !has_typars && !has_args {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::NAMED_PAT));
            consume_op(self);
            self.builder.finish_node(); // NAMED_PAT
            return;
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_PAT));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        consume_op(self);
        self.builder.finish_node(); // LONG_IDENT
        if has_typars {
            // `permit_empty = true`: a value binding's `explicitValTyparDeclsCore`
            // accepts an empty `let (!!)< > x = x`, matching the ident head.
            self.parse_typar_decls_postfix(true);
        }
        if self.at_name_pat_pairs() {
            self.parse_name_pat_pairs();
        } else {
            self.sweep_curried_arg_pats();
        }
        self.builder.finish_node(); // LONG_IDENT_PAT
    }

    /// Try to consume an atomic pattern at the current position. Returns
    /// `true` and emits the node + child tokens on success; `false`
    /// (without consuming or emitting anything) when the next filtered
    /// token isn't a recognised atomic-pat start. See
    /// [`Self::is_atomic_pat_start`] for the surface this covers.
    ///
    /// Depth-guarded ([`Self::with_depth_bool`]): this is the atomic-pattern
    /// dispatcher every nested / delimited pattern re-enters (a paren / list /
    /// record sub-pattern routes back through here), so bounding it bounds
    /// pattern nesting. On the limit — or once latched — it returns `false`,
    /// which terminates the `while self.try_emit_atomic_pat()` element loops.
    pub(super) fn try_emit_atomic_pat(&mut self) -> bool {
        self.with_depth_bool(Self::try_emit_atomic_pat_inner)
    }

    /// Emit `LONG_IDENT_PAT > LONG_IDENT > [IDENT_TOK(head) (DOT_TOK IDENT_TOK)*]`
    /// for a long-identifier pattern head, bumping the head token (an ident or
    /// `global`) as `IDENT_TOK` — matching FCS's
    /// `SynPat.LongIdent(SynLongIdent ids, …)` whose leading `idText` is the raw
    /// spelling (`"global"` for the reused-keyword head). The caller has
    /// positioned the cursor on the head and (for the `global` head) verified a
    /// dotted tail via [`Self::pat_head_has_dotted_tail`]; the tail is swept by
    /// [`Self::sweep_long_ident_dot_continuation`]. Shared by the ident and
    /// `global.`-rooted arms of [`Self::try_emit_atomic_pat_inner`].
    fn emit_rooted_long_ident_pat(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_PAT));
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        self.bump_into(SyntaxKind::IDENT_TOK);
        self.sweep_long_ident_dot_continuation();
        self.builder.finish_node(); // LONG_IDENT
        self.builder.finish_node(); // LONG_IDENT_PAT
    }

    fn try_emit_atomic_pat_inner(&mut self) -> bool {
        // Active-pattern name in atomic position — a curried arg
        // (`let f (|Foo|Bar|) = …`), a list/array element, etc. The whole
        // `(|…|)` is the name (FCS yields `Named("|Foo|Bar|")` with *no* `Paren`
        // wrapper, since the parens belong to the name), and an atomic
        // occurrence is always nullary, so it collapses to `NAMED_PAT` by the
        // same maybe-var rule as [`Self::try_emit_head_binding_pat_element`].
        // Detected before the `LParen` arm so the active-pattern `(` isn't taken
        // for an ordinary paren pattern.
        if self.at_active_pat_name() {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::NAMED_PAT));
            self.parse_active_pat_name();
            self.builder.finish_node(); // NAMED_PAT
            return true;
        }
        // A dotted path ending in an `opName` (`let f A.B.(+) y = …`, `[ A.(*) ]`):
        // FCS's `atomicPattern: atomicPatternLongIdent` reduction, which takes the
        // whole `pathOp` — including its `opName` final segment — but **no** typars
        // and **no** args (those belong to `constrPattern`, one rung up in
        // [`Self::try_emit_head_binding_pat_element`]). So the head is emitted
        // nullary: the following `y` stays the *next* curried argument of the
        // enclosing head rather than being swept into this one.
        if let Some(kind) = self.peek_dotted_opname_pat_head(false) {
            self.open_dotted_opname_pat_head(kind);
            self.builder.finish_node(); // LONG_IDENT_PAT
            return true;
        }
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Raw(Token::Underscore)), _)) => {
                // `_` is the wildcard. FCS's `UNDERSCORE DOT pathOp` (`_.M`) is a
                // sibling rooted long-ident head, but it is gated on the F# 4.7
                // `SingleUnderscorePattern` feature (`pars.fsy:2255`, a hard parse
                // error below 4.7); implementing it needs the language-version
                // machinery, so it lands with its gate in a later slice — see
                // [`Self::pat_head_has_dotted_tail`].
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::WILDCARD_PAT));
                self.bump_into(SyntaxKind::UNDERSCORE_TOK);
                self.builder.finish_node(); // WILDCARD_PAT
                true
            }
            Some((Ok(FilteredToken::Raw(Token::Global)), span)) => {
                // `global.N.Case` — FCS's `GLOBAL DOT pathOp` long-ident head
                // (`SynPat.LongIdent(["global"; …])`), the pattern-side twin of
                // the `global`-rooted expression path (`expr_atom.rs`). A *bare*
                // `global` is not a valid pattern (FCS FS0010), so without a
                // dotted tail we record a clean lossless error and consume the
                // keyword, mirroring FCS's rejection.
                if self.pat_head_has_dotted_tail() {
                    self.emit_rooted_long_ident_pat();
                } else {
                    self.errors.push(ParseError {
                        message: "expected `.` after `global` in a pattern".to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::IDENT_TOK);
                }
                true
            }
            Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), span)) => {
                // FCS's `atomicPattern → atomicPatternLongIdent` action
                // (`pars.fsy:3805-3818`) classifies the whole `pathOp` (an
                // `ident (DOT ident)*` long-ident path): a *multi-segment*
                // path (`Foo.Bar`) **or** a single uppercase ident →
                // `mkSynPatMaybeVar` → `SynPat.LongIdent(SynLongIdent(ids), …,
                // Pats[], …)`; a single *lowercase* ident → `SynPat.Named(…)`
                // (per `String.isLeadingIdentifierCharacterUpperCase`,
                // `Utilities/illib.fs:740`). We mirror that by emitting
                // `LONG_IDENT_PAT > LONG_IDENT > [IDENT_TOK (DOT_TOK IDENT_TOK)*]`
                // whenever the head is dotted or uppercase, and `NAMED_PAT >
                // IDENT_TOK` only for a bare lowercase single ident.
                // Function-form binding heads (with curried args) route
                // through `try_emit_head_binding_pat_element` and always emit
                // `LONG_IDENT_PAT` regardless of case (FCS does the same —
                // function-form is `applPat`, not `atomicPattern`), so this
                // classifier only fires at truly-atomic positions.
                let text = &self.source[span.clone()];
                if self.pat_head_has_dotted_tail() || ident_text_leads_uppercase(text) {
                    self.emit_rooted_long_ident_pat();
                } else {
                    self.builder
                        .start_node(FSharpLang::kind_to_raw(SyntaxKind::NAMED_PAT));
                    self.bump_into(SyntaxKind::IDENT_TOK);
                    self.builder.finish_node(); // NAMED_PAT
                }
                true
            }
            Some((Ok(FilteredToken::Raw(Token::Null)), _)) => {
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::NULL_PAT));
                self.bump_into(SyntaxKind::NULL_TOK);
                self.builder.finish_node(); // NULL_PAT
                true
            }
            Some((Ok(FilteredToken::Raw(Token::QMark)), _)) => {
                // `?ident` — `SynPat.OptionalVal` (FCS's `atomicPattern: QMARK
                // ident`, `pars.fsy:3802`). Open `OPTIONAL_VAL_PAT`, bump the `?`
                // sigil, then the named ident — plain or backtick-quoted, both
                // bumping `IDENT_TOK` so the `OptionalValPat::ident` accessor
                // (backtick-stripping) matches FCS's `Ident.idText`. Whitespace
                // between the `?` and the ident is ordinary trivia (FCS's grammar
                // imposes no adjacency), so a spaced `? x` parses identically.
                //
                // FCS has no bare-`QMARK` production, so a `?` not followed by an
                // ident is a parse error; we record it but still emit the node
                // (without the ident child) so the round-trip stays lossless and
                // the caller's curried-arg sweep cannot spin on the consumed `?`.
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::OPTIONAL_VAL_PAT));
                self.bump_into(SyntaxKind::QMARK_TOK);
                // The ident gate consults *both* streams, the same dual
                // discipline the function-form promotion uses: the **filtered**
                // peek must be a raw ident (rejecting a layout virtual parked
                // between `?` and an offside-laid-out name), and the **raw**
                // lookahead must be one too (rejecting a LexFilter-swallowed
                // closer — in `let f (?) y = y` the `)` is gone from the filtered
                // stream, so a filtered-only check would see `y` past it and pull
                // it into this node, draining the real `)` as ERROR and corrupting
                // the next argument). Requiring both leaves the `)` to close the
                // paren and `y` to survive as the next curried arg.
                let ident_next = matches!(
                    self.peek(),
                    Some((
                        Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))),
                        _
                    ))
                ) && matches!(
                    self.next_non_trivia_raw_at_pos(),
                    Some(Token::Ident(_) | Token::QuotedIdent(_))
                );
                if ident_next {
                    self.bump_into(SyntaxKind::IDENT_TOK);
                } else {
                    let span = self
                        .peek()
                        .map(|(_, s)| s.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message: "expected an identifier after `?` in a pattern".to_string(),
                        span,
                    });
                }
                self.builder.finish_node(); // OPTIONAL_VAL_PAT
                true
            }
            Some((Ok(FilteredToken::Raw(Token::LParen)), _)) => {
                // Parenthesised operator name (`(+)`, `(>>>&)`, spaced `( * )`)
                // at a truly-atomic position — a curried *argument* (`let f (+) =
                // …`) or a paren/clause/list element (`((+))`, `match x with (+)
                // -> …`). FCS's singleton-lowercase `atomicPattern:
                // atomicPatternLongIdent` reduction yields `SynPat.Named(SynIdent(op
                // …))`: the operator's mangled `op_*` idText with the source
                // spelling in its `OriginalNotationWithParen` trivia. We emit
                // `NAMED_PAT > [LPAREN_TOK, IDENT_TOK(op), RPAREN_TOK]` — the
                // pattern analogue of the expression-side operator-value — whose
                // `NamedPat::ident` accessor reads the inner `IDENT_TOK`; the
                // differential normaliser compares the source spelling on both
                // sides. `at_paren_op_value_pat` (not the expression
                // `at_paren_op_value`) admits the spaced `( * )` multiply name —
                // pattern position has no `IndexRange` wildcard to collide with.
                // The *applied* / typar'd forms (`let (+) a b = …`,
                // `let (!!)<'T> … `) are handled one rung up in
                // `try_emit_head_binding_pat_element`.
                if self.at_paren_op_value_pat(self.pos) {
                    self.builder
                        .start_node(FSharpLang::kind_to_raw(SyntaxKind::NAMED_PAT));
                    self.consume_paren_op_value();
                    self.builder.finish_node(); // NAMED_PAT
                } else {
                    self.parse_paren_pat();
                }
                true
            }
            Some((Ok(FilteredToken::Raw(Token::LParenStarRParen)), _)) => {
                // The glued `(*)` multiply operator-value as an *argument* /
                // element atomic pattern (`let f (*) = …`, `let (+) (*) = …`).
                // The lexer fuses `(*)` into one token (it would otherwise open a
                // block comment), so — unlike the general `( op )` / spaced
                // `( * )` forms on the `LParen` arm above — it needs its own
                // dispatch. FCS's `opName: LPAREN_STAR_RPAREN` →
                // `SynPat.Named(op_Multiply, "*")`; emit `NAMED_PAT > [LPAREN_TOK,
                // IDENT_TOK("*"), RPAREN_TOK]` via the shared
                // `consume_star_op_value`. The *head* spelling (`let (*) a b = …`)
                // is handled in `try_emit_head_binding_pat_element`.
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::NAMED_PAT));
                self.consume_star_op_value();
                self.builder.finish_node(); // NAMED_PAT
                true
            }
            Some((Ok(FilteredToken::Raw(Token::LBrack | Token::LBrackBar)), _)) => {
                self.parse_array_or_list_pat();
                true
            }
            Some((Ok(FilteredToken::Raw(Token::LBrace)), _)) => {
                self.parse_record_pat();
                true
            }
            Some((Ok(FilteredToken::Raw(Token::Struct)), struct_span)) => {
                // `struct (p1, p2, …)` → the struct-tuple pattern (FCS's
                // `STRUCT LPAREN tupleParenPatternElements rparen`,
                // `pars.fsy:3853`). The only `STRUCT`-led atomic pattern, so a
                // following `(` is required; anything else is a clean error
                // (there is no struct-anon-record *pattern* form, unlike the
                // expression side's `struct {| … |}`).
                //
                // The `(` lookahead consults *both* streams (the same dual
                // discipline as the function-form promotion), because each
                // rejects a hazard the other misses:
                //   * the **raw** lookahead (`next_non_trivia_raw_after`, past
                //     the `struct` span) surfaces a LexFilter-swallowed `)` that
                //     is gone from the filtered stream — for a malformed
                //     `(struct)` a filtered-only probe would see the *next*
                //     argument's `(` past the swallowed close
                //     (`let f (struct) (x, y) = …`) and parse a struct tuple
                //     across the closed paren, draining the real `)` as `ERROR`;
                //     and
                //   * the **filtered** lookahead (`next_non_trivia_filtered_after_pos`)
                //     surfaces a layout virtual (`Virtual::DeclEnd`/`BlockSep`)
                //     that the raw stream skips — for an offside break
                //     `let struct⏎(a, b) = …` a raw-only probe would see the
                //     next line's `(` past the declaration boundary and bump it
                //     (and the virtual) as the struct tuple's `(`.
                // Requiring a real `(` on both lets an adjacent `struct(a, b)` or
                // spaced `struct (a, b)` parse while either recovery hazard
                // declines cleanly.
                if matches!(
                    self.next_non_trivia_raw_after(struct_span.end),
                    Some(Token::LParen)
                ) && matches!(
                    self.next_non_trivia_filtered_after_pos(),
                    Some(FilteredToken::Raw(Token::LParen))
                ) {
                    self.parse_struct_tuple_pat();
                } else {
                    self.errors.push(ParseError {
                        message: "expected `(` after `struct` in a pattern".to_string(),
                        span: struct_span,
                    });
                    // Consume the `struct` so the round-trip stays lossless and
                    // the caller's loop cannot spin on it.
                    self.bump_into(SyntaxKind::STRUCT_TOK);
                }
                true
            }
            Some((Ok(FilteredToken::Raw(Token::LQuote | Token::LQuoteRaw)), _)) => {
                // A code quotation `<@ … @>` in pattern position — FCS's
                // `atomicPattern: quoteExpr` (`pars.fsy:3776`) →
                // `SynPat.QuoteExpr(expr, range)`, whose inner `expr` is a full
                // `SynExpr.Quote`. Wrap the shared quotation parser (which emits
                // the `QUOTE_EXPR` node) in a `QUOTE_PAT`, mirroring FCS's
                // `SynPat` wrapper around its `SynExpr`.
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::QUOTE_PAT));
                self.parse_quote_expr();
                self.builder.finish_node(); // QUOTE_PAT
                true
            }
            Some((Ok(FilteredToken::Raw(t)), _)) if raw_starts_const_payload(&t) => {
                self.parse_const_pat();
                true
            }
            _ => false,
        }
    }

    /// Parse a struct-tuple pattern `struct (p1, p2, …)` →
    /// [`SyntaxKind::TUPLE_PAT`] carrying a leading `STRUCT_TOK`, FCS's
    /// `STRUCT LPAREN tupleParenPatternElements rparen` →
    /// `SynPat.Tuple(isStruct = true, …)` (`pars.fsy:3853`). The pattern
    /// analogue of [`Self::parse_struct_tuple_expr`]; the cursor is at the
    /// `struct` keyword and the caller has verified a `(` follows.
    ///
    /// Unlike a regular `(p1, p2)` (which is `Paren(Tuple(false, …))`), the
    /// parens belong to *this* node and there is no `Paren` wrapper — the
    /// `STRUCT_TOK`, parens, element patterns, and `COMMA_TOK` separators sit
    /// directly under one `TUPLE_PAT`, and [`crate::syntax::TuplePat::is_struct`]
    /// reads the `STRUCT_TOK`. FCS requires ≥2 elements (`struct (p)` /
    /// `struct ()` are parse errors), so a missing comma after the first element
    /// is reported.
    ///
    /// Each element is a `tupleParenPatternElements` member — a `parenPattern`
    /// ([`Self::emit_paren_pat_element`], i.e. an applPat plus an optional
    /// per-element `: type`) followed by a tail climb at `PAT_BP_COMMA + 1`,
    /// which captures the operators tighter than the tuple comma (`::`, `&`)
    /// while the struct loop owns the commas. A top-level `as`/`|` *inside* a
    /// struct-tuple element is **not** captured here — FCS treats those forms as
    /// parse errors (final-element `as`, any `|`) or surprising whole-tuple
    /// nesting (non-final `as`), all rare/invalid; they fall through to a clean
    /// error rather than a wrong tree, the same disposition as the deferred
    /// `global.`/`_.`-rooted pattern heads.
    pub(super) fn parse_struct_tuple_pat(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TUPLE_PAT));
        self.bump_into(SyntaxKind::STRUCT_TOK);
        // The opening `(` is a real filtered token (only the closing `)` is
        // swallowed); `bump_into` drains the `struct`/`(` trivia before it.
        self.bump_into(SyntaxKind::LPAREN_TOK);
        // Drain raw trivia between `(` and the first element so it attaches to
        // `TUPLE_PAT` rather than landing inside the first element node.
        if let Some((_, next_span)) = self.peek() {
            let start = next_span.start;
            self.drain_raw_up_to(start);
        }

        // First element — a `parenPattern` whose tail captures everything
        // tighter than the comma.
        if self.emit_struct_tuple_pat_element() {
            // FCS's struct tuple needs ≥2 elements; a missing comma here
            // (`struct (a)`) is the "Unexpected symbol ')' in pattern" error.
            if !self.at_tuple_continuation() {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "a struct tuple pattern needs at least two elements".to_string(),
                    span,
                });
            }

            // Comma-separated remaining elements (mirrors `parse_struct_tuple_expr`,
            // stepping over offside `Virtual::BlockSep` between elements).
            while self.at_tuple_continuation() {
                self.bump_into(SyntaxKind::COMMA_TOK);
                self.drain_block_sep_after_tuple_comma();
                if !self.emit_struct_tuple_pat_element() {
                    // Trailing comma / missing element (`struct (a,)`): FCS
                    // reports a missing pattern. Mirror the expr tuple loop so a
                    // trailing comma is a clean error, not silently accepted.
                    let span = self
                        .peek()
                        .map(|(_, s)| s.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message: "expected pattern after `,` in struct tuple pattern".to_string(),
                        span,
                    });
                    break;
                }
            }
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a pattern inside `struct (…)`".to_string(),
                span,
            });
        }

        // The closing `)` is swallowed by the lex-filter, recovered off the raw
        // stream like a paren pattern's.
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        self.builder.finish_node(); // TUPLE_PAT
    }

    /// Emit one element of a struct-tuple pattern — a `tupleParenPatternElements`
    /// member. The element is a `parenPattern` ([`Self::emit_pat_atom`] in
    /// [`PatCtx::Paren`], i.e. an applPat plus an optional per-element `: type`,
    /// with a leading `[<…>]` attribute admitted) whose tail is climbed at
    /// `PAT_BP_COMMA + 1`: this captures the operators that bind tighter than the
    /// comma (`::`, `&`) but stops at the comma (owned by the struct loop) and at
    /// the looser `as`/`|`. Returns `false` (without consuming) when the cursor
    /// isn't at a pattern start — the struct loop turns that into a clean error.
    ///
    /// Dispatches through [`Self::emit_pat_atom`] (not [`Self::emit_paren_pat_element`]
    /// directly) precisely so the element emit is **raw-gated** like the normal
    /// tuple climber: a LexFilter-swallowed `)` is gone from the filtered stream,
    /// so for a malformed `struct (a,) x` the filtered cursor sits at `x` (the
    /// next curried arg, past the swallowed close) while the raw cursor is still
    /// at `)`. The raw-start guard inside `emit_pat_atom` declines, the missing
    /// element is reported at the `)`, and `x` survives intact.
    fn emit_struct_tuple_pat_element(&mut self) -> bool {
        let cp = self.builder.checkpoint();
        if !self.emit_pat_atom(PatCtx::Paren) {
            return false;
        }
        self.climb_pat_tail(cp, Self::PAT_BP_COMMA + 1, PatCtx::Paren);
        true
    }

    /// `SynPat.Const` for non-unit literals — open `CONST_PAT` around a
    /// [`Self::parse_const_payload`] dispatch. Mirrors
    /// [`Self::parse_const_expr`]; the same payload helper covers
    /// numeric/string/char/bool literals. The unit form `()` is NOT
    /// reached here on the pattern surface — FCS wraps unit-patterns in
    /// `SynPat.Paren(SynPat.Const(SynConst.Unit, _), _)`
    /// (`pars.fsy:3832`), and we follow suit by routing every `LParen`
    /// at pattern position through [`Self::parse_paren_pat`]. Caller
    /// must have confirmed the current token is accepted by
    /// [`raw_starts_const_payload`] *and* is not `LParen`.
    pub(super) fn parse_const_pat(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::CONST_PAT));
        self.parse_const_payload();
        self.builder.finish_node();
    }

    /// `SynPat.Paren` — open `PAREN_PAT` around an inner pat (or, for
    /// the unit case, a synthetic empty `CONST_PAT` standing for
    /// `SynConst.Unit`). Shape:
    /// `PAREN_PAT > [LPAREN_TOK, <inner-pat>, RPAREN_TOK]` with trivia
    /// interleaved. Caller must have verified the current token is
    /// `LParen`.
    ///
    /// FCS's `simplePatExpr`/`atomicPattern` rule (`pars.fsy:3832`) is
    /// `LPAREN parenPatternBody rparen → SynPat.Paren($2 m, m)`, with
    /// `parenPatternBody → /* empty */ → SynPat.Const(SynConst.Unit, m)`
    /// (`pars.fsy:3873`). So `()` at pattern position is always
    /// `Paren(Const(Unit))`, unlike the expression side where it's a
    /// bare `Const(Unit)`. The inner empty `CONST_PAT` we emit has no
    /// token children — the source `(` and `)` belong to the outer
    /// `PAREN_PAT`.
    ///
    /// LexFilter rewrites the closing `)` to
    /// `RPAREN_*_COMING_SOON`/`RPAREN_IS_HERE` markers that map to
    /// `FSharpTokenKind.None`, so the raw `RParen` never reaches the
    /// filtered stream; we drain it via
    /// [`Self::bump_swallowed_rparen`], mirroring
    /// [`Self::parse_paren_expr`].
    ///
    /// 6.1 only descends through one atomic pat inside the parens.
    /// Typed pat, tuple, or, ands, etc. — phases 6.2+ — aren't
    /// reachable here yet. They'll land naturally when `parse_pat`
    /// grows beyond the atomic case.
    pub(super) fn parse_paren_pat(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::PAREN_PAT));
        self.bump_into(SyntaxKind::LPAREN_TOK);
        // Disambiguate empty (`()` → unit) from non-empty (`(pat)`).
        // The closing `)` is swallowed by LexFilter, so we peek the
        // raw stream rather than the filtered one. Check this *before*
        // draining any leading trivia: in the unit case `next_filtered`
        // sits past the `)`, and draining up to it would swallow the
        // `)` as ERROR.
        if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::RParen)) {
            // Empty body — synthetic `CONST_PAT` with no token
            // children, mirroring FCS's `SynConst.Unit` placeholder
            // (`pars.fsy:3873`). Any inter-paren trivia (e.g. `( )`)
            // lands under `PAREN_PAT` via `bump_swallowed_rparen`'s
            // own trivia drain.
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::CONST_PAT));
            self.builder.finish_node();
        } else {
            // Drain raw trivia between `(` and the inner pattern so it
            // attaches to `PAREN_PAT` rather than landing inside the
            // inner node. Mirrors `parse_paren_expr`.
            if let Some((_, next_span)) = self.peek() {
                let start = next_span.start;
                self.drain_raw_up_to(start);
            }
            // Phase 6.2/6.3: each tuple element inside the parens is an
            // atomic pat optionally followed by `: <type>` — typed-pat
            // binds *per element*, not around the whole tuple. FCS
            // reaches the tuple form via `parenPattern → tuplePat` and
            // each tuple element via `tuplePat → patternAndTypeOrThisExpr`,
            // where the typed form is per-element (`pars.fsy:3929`). So
            // `(x, y : int)` projects to
            // `Paren(Tuple([Named x, Typed(Named y, int)]))`, with the
            // colon attached to the last element rather than the
            // surrounding tuple. The `tuple_cp` sits *inside* `PAREN_PAT`
            // so the retroactive `TUPLE_PAT` wrap excludes `LPAREN_TOK`
            // and the leading trivia.
            //
            // The tuple-comma sweep peeks the raw stream (not filtered)
            // so a `,` after a swallowed `)` doesn't fire — same gating
            // discipline as `maybe_wrap_tuple_pat`.
            let tuple_cp = self.builder.checkpoint();
            if !self.emit_paren_pat_element() {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected pattern after `(`".to_string(),
                    span,
                });
            } else {
                // Wrap the first element in the tuple / `as` / trailing-`:`
                // operators that follow, in token order. `in_paren = true`
                // routes tuple elements through `emit_paren_pat_element` (per-
                // element `: t`) and enables the post-`as` typed-pat wrap. Re-
                // using `tuple_cp` keeps `LPAREN_TOK`/leading trivia outside the
                // wraps.
                self.wrap_pat_tail(tuple_cp, PatCtx::Paren);
            }
        }
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        self.builder.finish_node();
    }

    /// `SynPat.ArrayOrList(isArray, elementPats, range)` — a list `[ … ]`
    /// or array `[| … |]` pattern. Modelled on
    /// [`Self::parse_anon_recd_type`]: bump the opener, emit
    /// `;`/block-sep-separated elements, then bump the matching close token
    /// (or record an error). Each element is a full in-delimiter
    /// `parenPattern` — [`Self::emit_paren_pat_element`] (an applPat plus an
    /// optional per-element `: t`) followed by
    /// [`Self::wrap_pat_tail`]`(cp, true)` (the `,`-tuple / `as` /
    /// trailing-`:` ladder) — the same pair [`Self::parse_paren_pat`] runs
    /// for its single content.
    ///
    /// FCS grammar (`pars.fsy:3786-3790`, `4035-4043`): `LBRACK
    /// listPatternElements RBRACK` → `ArrayOrList(false, …)` and `LBRACK_BAR
    /// … BAR_RBRACK` → `ArrayOrList(true, …)`, where `listPatternElements`
    /// is `EMPTY | parenPattern opt_seps | parenPattern seps
    /// listPatternElements` (`seps` = `;` / `OBLOCKSEP`, runs allowed,
    /// trailing tolerated). Unlike anon-record types, an empty body `[]` /
    /// `[||]` is **valid** — no "expected element" error. The grammar has no
    /// leading-separator production, so a leading `;` records an error (an
    /// FCS parse error too).
    ///
    /// The element separator is `;`, not `,`: `[a, b]` is a *one*-element
    /// list whose element is the tuple `(a, b)` (the `,` is folded into the
    /// element by `wrap_pat_tail`), while `[a; b]` is a two-element list.
    ///
    /// Brackets are not swallowed by LexFilter (unlike `)`; see
    /// `lexfilter/mod.rs` "emitted unchanged"), so the `peek()`-based bumps
    /// are correct — no `bump_swallowed_*` dance. Caller must have verified
    /// the current token is `LBrack` or `LBrackBar`; any other token is
    /// `unreachable!`.
    pub(super) fn parse_array_or_list_pat(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::ARRAY_OR_LIST_PAT));
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
                unreachable!("parse_array_or_list_pat called without `[`/`[|`: {other:?}")
            }
        };

        // `[` and `[|` close on different tokens; the closure folds that in
        // so the element loop and the final bump share one predicate.
        let at_close = |p: &Self| {
            if is_array {
                matches!(
                    p.peek(),
                    Some((Ok(FilteredToken::Raw(Token::BarRBrack)), _)),
                )
            } else {
                matches!(p.peek(), Some((Ok(FilteredToken::Raw(Token::RBrack)), _)))
            }
        };

        if !at_close(self) {
            // First element — a full in-delimiter pattern.
            let cp = self.builder.checkpoint();
            if self.emit_paren_pat_element() {
                self.wrap_pat_tail(cp, PatCtx::Paren);
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected pattern in list/array pattern".to_string(),
                    span,
                });
            }

            // Subsequent elements after one `seps` group. FCS's `seps` is a
            // *single* group (`;`, `OBLOCKSEP`, `; OBLOCKSEP`, or `OBLOCKSEP ;`),
            // so a repeated separator (`[a; ; b]`) is a parse error; consuming
            // one group per gap (via `consume_one_seps_group`) leaves any extra
            // to trip the element parser's recovery, matching FCS. A trailing
            // group before the close is tolerated (`opt_seps`). The `]`/`|]`
            // closer is a real filtered token, so `at_close` is a plain peek; an
            // offside element is separated by an `OBLOCKSEP` (`[a⏎ b]`).
            while !at_close(self) && self.consume_one_seps_group(at_close) {
                if at_close(self) {
                    break;
                }
                let cp = self.builder.checkpoint();
                if !self.emit_paren_pat_element() {
                    let span = self
                        .peek()
                        .map(|(_, s)| s.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message: "expected pattern after `;` in list/array pattern".to_string(),
                        span,
                    });
                    break;
                }
                self.wrap_pat_tail(cp, PatCtx::Paren);
            }
        }

        if at_close(self) {
            if is_array {
                self.bump_into(SyntaxKind::BAR_RBRACK_TOK);
            } else {
                self.bump_into(SyntaxKind::RBRACK_TOK);
            }
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: format!(
                    "expected `{}` to close list/array pattern",
                    if is_array { "|]" } else { "]" }
                ),
                span,
            });
        }
        self.builder.finish_node();
    }

    /// `SynPat.Record(fieldPats, range)` — a record pattern `{ X = p; … }`.
    /// FCS grammar: `atomicPattern: LBRACE recordPatternElementsAux rbrace`
    /// (`pars.fsy:3780`); each field is `recordPatternElement: path EQUALS
    /// parenPattern` (`pars.fsy:4023`), separated by `;`/`OBLOCKSEP` runs
    /// (`seps_block`, trailing tolerated). Atomic-level — reached from
    /// [`Self::try_emit_atomic_pat`] and recognised by
    /// [`raw_starts_atomic_pat`], so it works at every pattern caller site.
    ///
    /// The `{` is emitted normally, but the closing `}` is **swallowed** by
    /// LexFilter (like `)`; see `lexfilter`), so it never reaches the filtered
    /// stream — the close-detection peeks the *raw* stream and the close token
    /// is reclaimed via [`Self::bump_swallowed_rbrace`]. The field separator
    /// is `;`, **not** `,`: a `,` is folded into one field's value as a tuple
    /// by [`Self::wrap_pat_tail`] (`{ X = a, b }` ⇒ one field `X = Tuple[a,b]`).
    ///
    /// Caller must have verified the current token is `Token::LBrace`.
    pub(super) fn parse_record_pat(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::RECORD_PAT));
        self.bump_into(SyntaxKind::LBRACE_TOK);

        // The close `}` is LexFilter-swallowed, so probe the raw stream.
        let at_close =
            |p: &Self| matches!(p.next_non_trivia_raw_at_pos(), Some(Token::RBrace) | None);

        // An empty `{ }` is a parse error: FCS has no empty-record production
        // and reaches an empty `SynPat.Record` only via its `LBRACE error
        // rbrace` recovery rule (unlike `[]`/`[||]`, which are valid). Diagnose
        // it against the `}`'s span. A missing `}` instead (EOF) is reported by
        // `bump_swallowed_rbrace`, so gate on the `}` actually being present.
        if let Some((Token::RBrace, brace_span)) = self.next_non_trivia_raw_at_pos_with_span() {
            self.errors.push(ParseError {
                message: "record pattern requires at least one field".to_string(),
                span: brace_span,
            });
        }

        if !at_close(self) {
            self.parse_record_pat_field();

            // Subsequent fields after one `seps_block` group. FCS's `seps_block`
            // is a *single* group, so a repeated separator (`{ F = a; ; G = b }`)
            // is a parse error; consuming one group per gap (via
            // `consume_one_seps_group`) leaves any extra to trip the field
            // parser's recovery. A trailing group before `}` is tolerated
            // (`opt_seps_block`).
            //
            // `at_close` probes the *raw* stream because our own `}` is
            // swallowed from the filtered stream: when this record is nested and
            // immediately followed by an *outer* `;`/layout separator
            // (`{ X = { Y = a }; Z = b }`, `[ { X = a }; y ]`), `peek()` already
            // shows that outer separator while the raw stream still holds our
            // `}`. Gating each iteration on `at_close` (and passing it to the
            // helper) keeps the outer separator from being drained as an inner
            // one. Mirrors `parse_record_body`.
            while !at_close(self) && self.consume_one_seps_group(at_close) {
                if at_close(self) {
                    break;
                }
                self.parse_record_pat_field();
            }
        }

        self.bump_swallowed_closer(
            SyntaxKind::RBRACE_TOK,
            |t| matches!(t, Token::RBrace),
            "}",
            "record pattern",
        );
        self.builder.finish_node(); // RECORD_PAT
    }

    /// One `RECORD_PAT_FIELD > [LONG_IDENT, EQUALS_TOK, <value parenPattern>]`
    /// — FCS's `recordPatternElement: path EQUALS parenPattern`. The field
    /// name is a `path` ([`Self::parse_long_ident_path`], so `{ M.X = p }` is
    /// qualified); the value is a full in-delimiter `parenPattern`
    /// ([`Self::emit_paren_pat_element`] + [`Self::wrap_pat_tail`]`(cp, true)`),
    /// the same pair every other in-delimiter pattern element uses.
    pub(super) fn parse_record_pat_field(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::RECORD_PAT_FIELD));
        self.parse_long_ident_path("{");

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
                message: "expected `=` after record-pattern field name".to_string(),
                span,
            });
        }

        let cp = self.builder.checkpoint();
        if self.emit_paren_pat_element() {
            self.wrap_pat_tail(cp, PatCtx::Paren);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected pattern after `=` in record pattern".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // RECORD_PAT_FIELD
    }

    /// `SynPat.IsInst(type, range)` — the dynamic type-test pattern `:? T`.
    /// FCS's `constrPattern: COLON_QMARK atomTypeOrAnonRecdType`
    /// (`pars.fsy:3729`). Bump the `:?` operator, then parse the tested type
    /// at the `atomTypeOrAnonRecdType` level — an atomic type (which includes
    /// the `Foo<…>` prefix-app) or an anonymous record type, *not* the full
    /// type grammar. FCS's three productions are `COLON_QMARK
    /// atomTypeOrAnonRecdType` (happy path), `COLON_QMARK recover`, and a bare
    /// `COLON_QMARK` (EOF): the latter two both record an error and yield
    /// `SynPat.IsInst(SynType.FromParseError, …)`. We mirror that by emitting
    /// the node with no type child plus a `ParseError` when no type-start
    /// follows; the missing `FromParseError` type is not projected, so the
    /// diff harness never sees it.
    ///
    /// Caller (`try_emit_head_binding_pat_element`) has verified the cursor is
    /// at a `Token::ColonQMark`.
    pub(super) fn parse_is_inst_pat(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::IS_INST_PAT));
        self.bump_into(SyntaxKind::COLON_QMARK_TOK);

        // Gate the type parse on the shared `peek_starts_type_or_anon_recd`
        // predicate, which rejects *both* failure modes that `parse_atomic_type`
        // can't absorb:
        //   * a layout virtual (`Virtual::BlockSep`/`BlockEnd`) parked at the
        //     filtered cursor — e.g. an offside list-element `[ :?⏎    int ]`,
        //     where a raw-only peek would skip the virtual, find `int`, and
        //     dispatch into `parse_atomic_type` with the cursor still on the
        //     virtual → its `unreachable!` arm; and
        //   * a LexFilter-swallowed `)` (e.g. `(:?)`), where the *filtered*
        //     cursor lands past the `)` but the raw stream still surfaces it,
        //     so the predicate's raw check correctly rejects it rather than
        //     over-reading the next pattern/expr token as a type.
        // When neither fires the node is left without a type child plus a
        // recoverable error, mirroring FCS's `COLON_QMARK recover` arm.
        if self.peek_starts_type_or_anon_recd() {
            self.parse_atom_type_or_anon_recd_type();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected type after `:?`".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // IS_INST_PAT
    }
}
