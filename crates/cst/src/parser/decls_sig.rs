//! Signature-file (`.fsi`) declaration productions: `val` specifications,
//! signature type definitions and their reprs, and signature member blocks.

use super::*;

impl<'src> Parser<'src> {
    /// Parse a signature-file `val` specification (phase 10.12a) into a
    /// [`SyntaxKind::VAL_DECL`] node — FCS's `valSpfn` (`pars.fsy:745`) yields
    /// `SynModuleSigDecl.Val(SynValSig, range)`. Shape
    /// `VAL_DECL > [VAL_TOK, VAL_SIG]`; the inner [`SyntaxKind::VAL_SIG`]
    /// (`[MUTABLE_TOK?, INLINE_TOK?, ACCESS_TOK?, IDENT_TOK, COLON_TOK, <type>]`)
    /// is the `SynValSig` carrier shared with the abstract slot (phase 9.10c),
    /// so it reuses the same [`ValSig`](crate::syntax::ValSig) facade /
    /// `fcs_val_sig` projector. The caller has verified the cursor is at the raw
    /// `val` keyword.
    ///
    /// The signature type routes through [`Self::parse_type_with_constraints`]
    /// (FCS's `topTypeWithTypeConstraints`, `pars.fsy:746`), so a bare type
    /// (`int`), an arrow (`int -> string`, curried), a tuple argument
    /// (`int * int -> bool`), a generic/applied type (`int list`), and a
    /// trailing `when` clause (`'T -> 'T when 'T : comparison` →
    /// `SynType.WithGlobalConstraints`, phase 10.12c) all parse. Named/optional
    /// parameter signatures (`x: int -> int` → `SynType.SignatureParameter`) are
    /// a later 10.12 slice and stop cleanly at the `:` here. Explicit typars
    /// (`val f<'T> : …`) and a `= <literal>` value (`optLiteralValueSpfn`) are
    /// likewise deferred — both reject losslessly (no `<`/`=` is consumed).
    pub(super) fn parse_val_sig_decl(&mut self) {
        self.parse_val_sig_decl_at(None);
    }

    /// As [`Self::parse_val_sig_decl`], but with `outer_cp = Some(checkpoint)`
    /// the caller has already emitted one or more leading `ATTRIBUTE_LIST`s
    /// (the attributed sig form `[<Literal>] val x : int`) after the checkpoint;
    /// the `VAL_DECL` is opened *at* that checkpoint so the attribute lists
    /// become its leading children (FCS homes them in `SynValSig.attributes`).
    /// `None` is the plain (unattributed) form.
    pub(super) fn parse_val_sig_decl_at(&mut self, outer_cp: Option<rowan::Checkpoint>) {
        // Keep leading trivia/comments as a sibling of VAL_DECL, matching the
        // convention in `parse_open_decl` / `parse_module_decl` / `parse_let_decl_at`
        // — at file start or after a same-line `;` separator there is no layout
        // virtual to drain it first, so without this the trivia (and the node
        // range) would start before the `val` keyword. (With `outer_cp` the
        // attribute parse already drained up to the `[<`, and the inter-attr/`val`
        // trivia is drained by the `bump_into(VAL_TOK)` below.)
        let val_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_val_sig_decl invoked without a peeked Token::Val");
        self.drain_raw_up_to(val_span.start);
        match outer_cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::VAL_DECL)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::VAL_DECL)),
        }
        self.bump_into(SyntaxKind::VAL_TOK);
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::VAL_SIG));
        // The `opt_inline opt_mutable opt_access` run after `val` (FCS's
        // `valSpfn`, `pars.fsy:746`). `inline` → `SynValSig.isInline`,
        // `mutable` → `isMutable`, an access modifier → `accessibility`; all
        // elided by the normaliser, so a lenient run of any of them is consumed.
        loop {
            match self.peek() {
                Some((Ok(FilteredToken::Raw(Token::Inline)), _)) => {
                    self.bump_into(SyntaxKind::INLINE_TOK);
                }
                Some((Ok(FilteredToken::Raw(Token::Mutable)), _)) => {
                    self.bump_into(SyntaxKind::MUTABLE_TOK);
                }
                Some((
                    Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
                    _,
                )) => {
                    self.bump_into(SyntaxKind::ACCESS_TOK);
                }
                _ => break,
            }
        }
        // The value name. FCS's `valSpfn` names the value through `opName`
        // (`pars.fsy`), which — beyond a plain identifier — admits a
        // parenthesised operator-value (`val (+) : …`, `val ( * ) : …`) and an
        // active-pattern name (`val (|Foo|_|) : …`). Reuse the binding-head
        // machinery verbatim: the operator emits `[LPAREN_TOK, IDENT_TOK(op),
        // RPAREN_TOK]` (the bare operator under `IDENT_TOK`, which the FCS-side
        // normaliser matches by unwrapping FCS's mangled `op_*` +
        // `OriginalNotationWithParen`), and the active-pattern name emits an
        // `ACTIVE_PAT_NAME` node (the same node a pattern/expression occurrence
        // uses, including its FS0623/FS0624 case-name diagnostics). The
        // active-pattern check comes first: both open with `(`, but only its
        // second token is the `|` that `peek_operator_head` excludes. Gate the
        // plain-ident arm on the *filtered* token so a pending layout close
        // (`OBLOCKEND`) is not seen through (cf. [`Self::parse_val_field_at`]).
        if self.at_active_pat_name() {
            self.parse_active_pat_name();
        } else if let Some(is_star) = self.peek_operator_head() {
            if is_star {
                self.consume_star_op_value();
            } else {
                self.consume_paren_op_value();
            }
        } else if matches!(
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
                message: "expected a value name after `val`".to_string(),
                span,
            });
        }
        // Optional postfix `<'T, …>` explicit value type parameters (FCS's
        // `opt_explicitValTyparDecls`, `pars.fsy:746` → `SynValSig.explicitTypeParams`,
        // a `SynValTyparDecls`). Reuse the phase-9.3 `parse_typar_decls_postfix`
        // (the same `TYPAR_DECLS` node a `type T<'a>` header uses), into the open
        // `VAL_SIG`; the inside-`<>` `when` constraint clause folds into the
        // `TYPAR_DECLS`' `PostfixList` exactly as a type header's does. The `<` is
        // adjacent (`f<'T>`, preceded by the `HighPrecedenceTyApp` virtual) or
        // spaced (a bare raw `Less`); right after the value name a `<` can only open
        // type parameters.
        //
        // `permit_empty = true`: unlike a type definition's `postfixTyparDecls`,
        // FCS's `valSpfn` `explicitValTyparDeclsCore` admits an *empty* core, so the
        // spaced `val f< > : int` is valid (with a non-adjacent-typars warning that
        // does not set `ParseHadErrors`). (The adjacent `val f<>` never reaches here
        // — the lexer reads `<>` as the not-equal operator, not a `Less`.)
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                    | FilteredToken::Raw(Token::Less(_))),
                _,
            ))
        ) {
            self.parse_typar_decls_postfix(true);
        }
        // The mandatory `: <type>` signature. FCS's `valSpfn` ends in
        // `COLON topTypeWithTypeConstraints` (`pars.fsy:746`), so a trailing
        // `when` clause folds the type into `SynType.WithGlobalConstraints`
        // (phase 10.12c) and labelled parameters (`x: int -> int`, phase 10.12b)
        // are admitted — route through `parse_type_with_constraints_top`, the
        // `topType` wrapper.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
            self.bump_into(SyntaxKind::COLON_TOK);
            self.parse_type_with_constraints_top();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `:` and a type in a `val` signature".to_string(),
                span,
            });
        }
        // Optional `= <literal>` value (FCS's `optLiteralValueSpfn = EQUALS
        // declExpr`, `pars.fsy:765` → `SynValSig.synExpr`, a `[<Literal>]` value's
        // RHS). LexFilter frames the RHS as `OBLOCKBEGIN declExpr OBLOCKEND` — the
        // same shape a `let` binding RHS uses — so reuse `parse_let_equals_rhs`: it
        // consumes the `=` + opening `OBLOCKBEGIN`, gathers the expression into the
        // open `VAL_SIG` (a one-sided seq block), and leaves the closing
        // `OBLOCKEND` for the enclosing sig-decl loop (so a following sibling spec
        // is reached). The RHS is a full `SynExpr` (usually a `Const`, but `1 + 2`
        // / `E.A` parse too).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Equals)), _))
        ) {
            self.parse_let_equals_rhs(false);
        }
        self.builder.finish_node(); // VAL_SIG
        self.builder.finish_node(); // VAL_DECL
    }

    /// Phase 10.14 — parse a single type-definition signature into the impl-side
    /// [`SyntaxKind::TYPE_DEFNS`] / [`SyntaxKind::TYPE_DEFN`] nodes
    /// (`SynModuleSigDecl.Types` holding one `SynTypeDefnSig`). The
    /// `SynTypeDefnSimpleRepr` forms are modelled: the abbreviation
    /// `type T = <ty>` (slice 1, a [`SyntaxKind::TYPE_ABBREV`] child), the
    /// opaque/bodyless `type T` (slice 2a, **no** repr child), and the record /
    /// union / enum reprs (slice 2b, `RECORD_REPR` / `UNION_REPR` / `ENUM_REPR`
    /// via the impl [`Self::parse_record_repr`] / [`Self::parse_union_or_enum_repr`]).
    /// An object-model body of `member`/`abstract`/`static member` / `val`-field /
    /// `inherit` / `interface` signatures (slices 3a/3b,
    /// `SynTypeDefnSigRepr.ObjectModel`) lands in an `OBJECT_MODEL_REPR` of
    /// [`SyntaxKind::MEMBER_SIG`] (and reused impl member) children, as does an
    /// explicit `class`/`struct`/`interface … end` body (slice 3c). A bodyless
    /// `with`-augmentation `type T with member …` (slice 4) keeps `repr()` absent
    /// (`Simple(None)`) and homes its member sigs in the *outer* `TYPE_DEFN`
    /// members slot, as does a trailing `with`/bare-member sig on a structural repr
    /// (`type R = {…} with member …`, slice 6 — the repr is kept, the members are
    /// outer). A `delegate of …` body (slice 7) parses into a
    /// [`SyntaxKind::DELEGATE_REPR`] (reusing the impl-side
    /// [`Self::parse_delegate_repr`]). Attributed member sigs (`[<…>] member …`,
    /// slice 8) thread the attribute lists into the member node. The
    /// `SynComponentInfo` header (name, type parameters, after-keyword attributes,
    /// after-decls `when` clause) reuses the impl-side
    /// [`Self::parse_type_defn_header`] / [`Self::parse_typar_constraints`].
    ///
    /// A residual member-sig kind the loop does not model (an attributed
    /// `inherit`/`interface`, or a malformed body) records one
    /// [`TYPE_SIG_UNSUPPORTED_BODY_ERROR`] diagnostic and skips the body
    /// ([`Self::parse_sig_type_defn_repr`]) — kept inside the open `TYPE_DEFN`
    /// so a nested spec (`type C =`⏎`  val x : int`) is not promoted to a
    /// top-level [`SyntaxKind::VAL_DECL`] (a phantom export). An
    /// `and`-continuation (`type A = …`⏎`and B = …`, slice 5) is consumed by
    /// [`Self::parse_sig_type_defn_at`]'s chain loop into the same `TYPE_DEFNS`
    /// group (mirroring the impl [`Self::parse_type_defn_at`]). Caller has
    /// verified, via [`Self::raw_leading_type_defn`], that a swallowed `type` sits
    /// at the raw cursor.
    pub(super) fn parse_sig_type_defn(&mut self) {
        self.parse_sig_type_defn_at(None);
    }

    /// As [`Self::parse_sig_type_defn`], but with `cp = Some(checkpoint)` the
    /// caller has already emitted one or more leading `ATTRIBUTE_LIST`s (the
    /// attributed sig form `[<Sealed>] type T`) after the checkpoint; both the
    /// `TYPE_DEFNS` group and its first `TYPE_DEFN` are opened *at* that
    /// checkpoint so the attribute lists become leading children of the
    /// `TYPE_DEFN` (FCS homes them in the `SynTypeDefnSig`'s
    /// `SynComponentInfo.attributes`). Mirrors the impl-side
    /// [`Self::parse_type_defn_at`]. `None` is the plain (unattributed) form.
    pub(super) fn parse_sig_type_defn_at(&mut self, cp: Option<rowan::Checkpoint>) {
        let (kw, type_span) = self
            .next_non_trivia_raw_at_pos_with_span()
            .expect("caller verified a swallowed `type`");
        debug_assert!(
            matches!(kw, Token::Type),
            "parse_sig_type_defn invoked without a swallowed raw `type`",
        );
        match cp {
            // Attributed form: the `ATTRIBUTE_LIST`s sit between the checkpoint
            // and here. Open *both* the group and the first definition at `cp`
            // so the attrs land as leading children of the `TYPE_DEFN`; the
            // `>]`→`type` whitespace belongs inside the definition (after the
            // attrs), so drain it once the node is open (mirror
            // `parse_type_defn_at`).
            Some(cp) => {
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TYPE_DEFNS));
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TYPE_DEFN));
                self.drain_raw_up_to(type_span.start);
            }
            // Plain form: leading trivia stays a sibling of the decl node
            // (mirror `parse_type_defn`). Claim the swallowed `type` directly
            // from the raw stream (it never reached the filtered stream, so
            // `bump_into` would mark it ERROR).
            None => {
                self.drain_raw_up_to(type_span.start);
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPE_DEFNS));
                self.builder
                    .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPE_DEFN));
            }
        }
        self.emit_text(SyntaxKind::TYPE_TOK, type_span.clone());
        self.raw_pos += 1;
        let (mut prev_closed, offside_attr) = self.parse_sig_type_defn_name_and_body();
        self.builder.finish_node(); // TYPE_DEFN

        // `and`-chained continuations (slice 5) — FCS's `tyconSpfn` chains via `AND
        // tyconSpfn`, so `type A = … and B = …` is one `SynModuleSigDecl.Types`
        // holding several `SynTypeDefnSig`s. Each continuation is its own
        // `TYPE_DEFN` leading with an `AND_TOK`; inter-definition trivia (the
        // newline before `and`) stays a `TYPE_DEFNS`-level sibling. This mirrors
        // the impl-side `parse_type_defn_at` loop verbatim (the lex-filter emits
        // identical virtuals for `.fs`/`.fsi`): a continuation is taken only when
        // the previous body's offside block closed (`prev_closed`) — a valid `and`
        // is offside on its own line, so the prior block closed before it — *or* in
        // the column-0 after-keyword-attribute regime (where bodies are blockless,
        // so `prev_closed` is uninformative). An *inline* `type A = int and B = …`
        // keeps the `and` inside the still-open first block (FCS rejects it), so
        // with neither condition met the `and` is left for the enclosing loop to
        // flag rather than splicing a bogus chain.
        let mut column0_regime = offside_attr;
        loop {
            if !prev_closed && !column0_regime {
                break;
            }
            let Some((Ok(FilteredToken::Raw(Token::And)), and_span)) = self.peek().cloned() else {
                break;
            };
            self.drain_raw_up_to(and_span.start);
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPE_DEFN));
            self.bump_into(SyntaxKind::AND_TOK);
            let (closed, offside_attr) = self.parse_sig_type_defn_name_and_body();
            prev_closed = closed;
            column0_regime |= offside_attr;
            self.builder.finish_node(); // TYPE_DEFN
        }

        self.builder.finish_node(); // TYPE_DEFNS
    }

    /// Parse one signature type definition's `SynComponentInfo` header (name +
    /// type parameters + after-keyword attributes) and its body, after the leading
    /// keyword (`TYPE_TOK` / `AND_TOK`) has been claimed. Shared by the head and
    /// `and`-chained definitions in [`Self::parse_sig_type_defn_at`] — the sig
    /// counterpart of the impl [`Self::parse_type_defn_name_and_body`] (a
    /// `SynTypeDefnSig` has no implicit primary constructor, so that step is
    /// absent). Returns `(closed_block, offside_attr)`: whether the body's offside
    /// block closed (see [`Self::parse_sig_type_defn_repr`]) and whether the header
    /// drained an after-keyword attribute's trailing offside `BlockSep` (the
    /// column-0 offside-name form `type [<A>]`⏎`T`, which yields a blockless body).
    fn parse_sig_type_defn_name_and_body(&mut self) -> (bool, bool) {
        // The `SynComponentInfo` header — name + optional type parameters +
        // after-keyword attributes (reused verbatim from the impl path).
        let offside_attr = self.parse_type_defn_header();
        // Optional after-decls `when …` constraint clause
        // (`SynComponentInfo.constraints`), before the repr's `=`.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::When)), _))) {
            self.parse_typar_constraints();
        }
        let closed_block = self.parse_sig_type_defn_repr();
        (closed_block, offside_attr)
    }

    /// Parse the body of a signature type definition (phase 10.14). The
    /// `SynTypeDefnSimpleRepr` forms are modelled, reusing the impl-side body
    /// parsers and the `=` → optional `OBLOCKBEGIN` → repr → closing `OBLOCKEND`
    /// framing of [`Self::parse_type_defn_repr`]: the `= <ty>` abbreviation
    /// (slice 1, [`SyntaxKind::TYPE_ABBREV`]), the opaque/bodyless `type T`
    /// (slice 2a — no `=`, no `with`: emit no repr node, no diagnostic), and the
    /// record / union / enum reprs (slice 2b, via [`Self::parse_record_repr`] /
    /// [`Self::parse_union_or_enum_repr`]); a lightweight object-model body of
    /// `member`/`abstract`/`static member` / `val`-field / `inherit` / `interface`
    /// signatures (slices 3a/3b, via [`Self::parse_sig_object_model_repr`]); an
    /// explicit `class`/`struct`/`interface … end` body (slice 3c, via
    /// [`Self::parse_sig_kind_marked_repr`]); a bodyless `with`-augmentation
    /// `type T with member …` (slice 4 — `Simple(None)` repr with the member sigs
    /// in the outer slot, via [`Self::parse_with_augmentation_members`]); and a
    /// trailing `with`/bare-member sig on a structural / abbreviation repr (slice 6
    /// — the repr is kept and the member sigs land in the outer slot, via
    /// [`Self::parse_sig_member_block_items`] / [`Self::parse_with_augmentation_members`]);
    /// a `delegate of …` body (slice 7, via [`Self::parse_delegate_repr`]); and
    /// attributed member sigs (slice 8, threaded through
    /// [`Self::parse_sig_member_block_items`]). A residual member-sig kind the loop
    /// does not model (an attributed `inherit`/`interface`, or a malformed body)
    /// records one [`TYPE_SIG_UNSUPPORTED_BODY_ERROR`] diagnostic and is skipped, so
    /// a member spec is not leaked as a top-level decl.
    ///
    /// Returns `closed_block` (mirroring the impl [`Self::parse_type_defn_repr`]):
    /// `true` iff the body's offside block opened **and** closed (or the body was
    /// bodyless/blockless, hence "complete"). [`Self::parse_sig_type_defn_at`]'s
    /// `and`-chain loop uses it to decide whether a following `and` continues the
    /// group — a single-line body leaves its block open, so an inline `and` is not
    /// chained.
    fn parse_sig_type_defn_repr(&mut self) -> bool {
        // A non-`=` body. FCS's `tyconSpfn` second alternative
        // (`typeNameInfo opt_classSpfn`) produces a **bodyless** type —
        // `SynTypeDefnSigRepr.Simple(SynTypeDefnSimpleRepr.None)`, the opaque
        // type `type T` (slice 2a) — but *only* when the spec is properly
        // terminated. The type is complete when the next token is a genuine
        // boundary: any layout-closing virtual (`OBLOCKSEP`/`OBLOCKEND`/
        // `ODECLEND`/…), a top separator (`;`/`;;`), an `and`-continuation
        // (handled by the caller's loop), or EOF. A bodyless `with`-augmentation
        // (`type T with member …`, slice 4) is handled first, below. Otherwise a
        // body / malformed continuation follows and is skipped with a diagnostic —
        // so it is not leaked as a top-level decl:
        //  * an offside member block (`OBLOCKBEGIN`) with no `=` — an unsupported
        //    blockless member body;
        //  * **any other raw token directly abutting the header** — an *indented*
        //    spec such as `type T`⏎`  val x : int`, which the lex-filter leaves
        //    at the cursor with no separating virtual (per the offside rule it
        //    continues the type's line) — which FCS rejects. Accepting it would
        //    promote the `val` to a phantom top-level export.
        if !matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Equals)), _))
        ) {
            // A `with`-augmentation on a bodyless type — `type T with member …`
            // (FCS's `tyconSpfn` second alternative `typeNameInfo opt_classSpfn`,
            // slice 4). FCS lowers it to `Simple(SynTypeDefnSimpleRepr.None)` with
            // the augmentation's member *sigs* in the **outer**
            // `SynTypeDefnSig.members` slot — *unlike* the impl side, where
            // `type T with member …` is an `ObjectModel(Augmentation)` repr. Two
            // LexFilter forms, exactly as the 10.15 exception augmentation:
            //  * `Raw(with) OBLOCKBEGIN … OBLOCKEND` — every *supported* member-sig
            //    start. The `with` is a plain [`SyntaxKind::WITH_TOK`] direct child
            //    of the open `TYPE_DEFN` (no `OBJECT_MODEL_REPR` marker), leaving
            //    `repr()` absent → `None`, and the member sigs become direct
            //    `MEMBER_SIG` children via the shared
            //    [`Self::parse_with_augmentation_members`] (`sig = true`).
            //  * `OWITH … OEND` ([`Virtual::With`]) — emitted when the first augment
            //    member begins on the *same line* with a leading `[<…>]` attribute
            //    or access modifier (`type T with [<A>] member …` / `with private
            //    member …`). Those forms are a later slice (FCS itself errors), so
            //    the block is contained as ERROR rather than left to escape the
            //    `TYPE_DEFN` and leak the member as a sibling spec.
            match self.peek() {
                Some((Ok(FilteredToken::Raw(Token::With)), _)) => {
                    self.bump_into(SyntaxKind::WITH_TOK);
                    // The augment's closed-block flag drives a following `and`
                    // continuation (`type T with member … and U = …`).
                    return self.parse_with_augmentation_members(true, true);
                }
                Some((Ok(FilteredToken::Virtual(Virtual::With)), sp)) => {
                    let span = sp.clone();
                    self.errors.push(ParseError {
                        message: "this member signature in a `with` augmentation is not yet \
                                  supported (later phase-10 slice)"
                            .to_string(),
                        span,
                    });
                    self.skip_owith_block_as_error();
                    // The `OWITH … OEND` block is fully consumed; treat the (deferred,
                    // already-flagged) spec as complete so recovery is uniform.
                    return true;
                }
                _ => {}
            }
            let opaque_complete = match self.peek() {
                None => true,
                // An offside member block or any other raw token directly
                // continuing the header line → a body we do not model yet / a
                // malformed continuation. (A `with` is handled above.)
                Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _)) => false,
                // A top separator or `and`-continuation terminates the opaque
                // type cleanly (the caller's loop handles `;`/`;;`/`and`).
                Some((Ok(FilteredToken::Raw(Token::Semi | Token::SemiSemi | Token::And)), _)) => {
                    true
                }
                // An abutting `val` spec — the `val` is *indented* under the
                // bodyless header, so the lex-filter emits no separating layout
                // virtual (it abuts `Name`). FCS nonetheless closes the opaque
                // type and parses the `val` as a *module-level*
                // `SynModuleSigDecl.Val`, promoted out of the type (a bodyless
                // `type Shape`⏎`  val (|…|) : …`, `ProvidedTypes.fsi`). Leave the
                // `val` at the cursor for the enclosing module-sig-decl loop; the
                // opaque type is complete. Only `val` promotes this way — every
                // *other* abutting raw token (`member`, `abstract`, `inherit`, …)
                // is not a valid module-sig-decl, so it stays on the `false` arm
                // below and is skipped, matching FCS's rejection.
                Some((Ok(FilteredToken::Raw(Token::Val)), _)) => true,
                // The same, with a *leading attribute run* — `[<A>] val X`. FCS
                // promotes the attributed `val` to a module-level `Val` too, so
                // look past the attribute list(s) for the `val`; an attributed
                // *non*-`val` (`[<A>] type`/`member`) is not a valid module decl
                // and falls through to the `false` arm (FCS rejects it).
                Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
                    if self.attributed_val_follows_from(self.pos) =>
                {
                    true
                }
                // Any *other* raw token abuts the header with no separating
                // layout virtual — an indented continuation (FCS errors).
                Some((Ok(FilteredToken::Raw(_)), _)) => false,
                // A layout-closing virtual ends the declaration; a lex-error
                // token is left for the caller's recovery (no second diagnostic).
                Some((Ok(FilteredToken::Virtual(_)), _)) | Some((Err(_), _)) => true,
            };
            if !opaque_complete {
                self.push_type_sig_unsupported_body_error();
                self.skip_type_sig_body_remainder();
            }
            // A bodyless/opaque type is "complete" (no offside block to leave
            // open), so a newline `and` continuation is taken — `type A`⏎`and B`
            // stays one group, matching FCS.
            return true;
        }
        self.bump_into(SyntaxKind::EQUALS_TOK);
        // Opening `OBLOCKBEGIN` — `bump_into(ERROR)` exactly as the impl-side
        // `parse_type_defn_repr`: it emits a zero-width ERROR for the virtual
        // *and* drains the post-`=` whitespace as the placeholder's leading
        // trivia, so `parse_type` starts tight on the RHS and `TYPE_ABBREV`'s
        // range is not shifted onto that whitespace. (The *closing* `OBLOCKEND`
        // below stays a manual zero-width bump — there the raw cursor must not
        // advance, to preserve a following swallowed `type`/`and` keyword.)
        // Absent on a single-line body and in the column-0 after-keyword-
        // attribute regime (`type [<A>]`⏎`C =`⏎`  …`), where the body is
        // blockless.
        let opened_block = if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
            true
        } else {
            false
        };
        // Dispatch on the repr's first token, reusing the impl-side predicates
        // and body parsers. The `SynTypeDefnSimpleRepr` forms — abbreviation
        // (slice 1), record / union / enum (slice 2b) — share FCS's
        // `tyconDefnOrSpfnSimpleRepr` grammar, so the `TYPE_ABBREV` /
        // `RECORD_REPR` / `UNION_REPR` / `ENUM_REPR` nodes and the normaliser
        // are reused unchanged. The object-model forms (explicit
        // `class`/`struct`/`interface … end`, a bare `member …` body — including an
        // attributed `[<…>] member` first item, slice 8) and the `delegate of …`
        // form (slice 7) are FCS's `SynTypeDefnSigRepr.ObjectModel`.
        //
        // `is_delegate` flags the `delegate of …` body: FCS forbids any
        // augmentation on a delegate (`parsAugmentationsIllegalOnDelegateType`), so
        // a trailing bare-member / `with` run below is flagged rather than accepted.
        let mut is_delegate = false;
        if self.peek_is_record_repr_start() {
            self.parse_record_repr();
        } else if self.peek_is_union_or_enum_repr_start() {
            // Returns whether bare trailing members are admitted; the sig side
            // skips any trailing member sigs uniformly below, so it is unused.
            let _ = self.parse_union_or_enum_repr();
        } else if self.peek_is_inline_il_repr_start() {
            // FSharp.Core's inline-IL type body — `( # "instr" # )`
            // (`SynTypeDefnSimpleRepr.LibraryOnlyILAssembly`). Appears in
            // `prim-types.fsi` (`type byref<'T> = (# "!0&" #)`) just as in the
            // `.fs`, so the sig dispatch needs it too — before the abbreviation
            // arm, which `peek_starts_type` would otherwise claim (choking on
            // the `#`). Reuses the impl-side `INLINE_IL_REPR` parser / node.
            self.parse_inline_il_repr();
        } else if !self.peek_is_sig_kind_marked_repr_start()
            && !self.peek_is_delegate_repr_start()
            && !self.peek_is_object_model_start()
            && self.peek_starts_type()
        {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPE_ABBREV));
            self.parse_type();
            self.builder.finish_node(); // TYPE_ABBREV
        } else if opened_block && self.peek_is_sig_member_block_start() {
            // An object-model body of member signatures — `member`/`abstract`/
            // `static member` (slice 3a) plus `inherit T` / `interface I` /
            // `val x : T` (slice 3b) — FCS's `SynTypeDefnSigRepr.ObjectModel(…,
            // memberSigs, …)`. Parses the member sigs into an `OBJECT_MODEL_REPR`
            // (reusing the impl repr + member nodes, so the normaliser projects
            // them via the shared `ObjectModel` / `normalise_member` arms). A
            // trailing member-sig kind not yet modelled (a nested-type sig, or an
            // attributed `inherit`/`interface`) ends the loop and is skipped by the
            // trailing-body handling below.
            //
            // Gated on `opened_block`: a member body is only modelled when a real
            // offside block was opened (`type T =`⏎`  member …`). The sole
            // *blockless* member body is the column-0 after-keyword-attribute
            // regime (`type [<A>]`⏎`C =`⏎`  member …`), which is invalid F# (FCS
            // drops the whole file) and whose layout cannot tell a same-column
            // member from a dedented sibling (both are `OBLOCKSEP` then the item);
            // routing it to the skip branch below instead skips the col-aligned
            // member run and preserves any genuine sibling.
            self.parse_sig_object_model_repr();
        } else if self.peek_is_sig_kind_marked_repr_start() {
            // An explicit-kind object-model body — `class … end` / `struct … end`
            // / `interface … end` (slice 3c), FCS's
            // `SynTypeDefnSigRepr.ObjectModel(Class|Struct|Interface, memberSigs,
            // _)` — or the verbose `begin … end` body, whose kind is
            // `Unspecified` (`begin`/`end` pure delimiters, dropped from the AST;
            // `test.fsi`'s `AbstractType`). The member sigs reuse the 3a/3b loop;
            // the kind + projection are already in the normaliser. Unlike a bare
            // member body this needs no `opened_block` gate — the explicit
            // `end` delimits the body, so there is no blockless column-0 ambiguity.
            self.parse_sig_kind_marked_repr();
            // The explicit-`end` form emits an extra `OBLOCKSEP` *after* `end`,
            // before the outer body block's closing `OBLOCKEND` (FCS's
            // `explicit_end_form`, mirrored from the impl `parse_type_defn_repr`).
            // Skip it (zero-width) when the outer block was opened — otherwise the
            // trailing handling below sees a non-`OBLOCKEND` virtual at the cursor
            // and wrongly skips an otherwise-valid body as unsupported.
            if opened_block
                && matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                )
            {
                self.builder
                    .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                self.pos += 1;
            }
        } else if self.peek_is_delegate_repr_start() {
            // A delegate body — `delegate of <topType>` (slice 7). FCS lowers it to
            // `ObjectModel(Delegate(ty, arity), [Invoke], _)`; we keep the surface
            // `DELEGATE_REPR` node, reusing the impl-side `parse_delegate_repr` and
            // the `NormalisedTypeRepr::Delegate` projection. The body-closing
            // `OBLOCKEND` is left for the `closed_block` handling below.
            self.parse_delegate_repr();
            is_delegate = true;
        } else {
            // A body we do not model yet whose *first* item is not a member sig:
            // a `new`/property/nested-type member sig, or an attributed
            // `[<…>] member` (attributed members are deferred). Skip
            // the body: a block body's `OBLOCKBEGIN` is
            // already consumed, so drain its interior up to (and including) the
            // matching `OBLOCKEND`; a blockless body (the column-0 regime) is
            // drained token-by-token up to the next layout virtual. Either way
            // the body close is handled here, so return.
            self.push_type_sig_unsupported_body_error();
            if opened_block {
                self.skip_offside_block_interior_as_error();
            } else {
                self.skip_type_sig_body_remainder();
            }
            // The skip consumed the body up to (and through) its close, so the
            // spec is complete for the `and`-chain gate.
            return true;
        }
        // Bare trailing member sigs (slice 6) — FCS's `#light` `tyconSpfnRhsBlock`
        // admits a `classSpfnMembers` run after a structural / abbreviation repr
        // (`pars.fsy:838`), routed to the **outer** `SynTypeDefnSig.members` slot.
        // They arrive either behind an `OBLOCKSEP` (the offside `type R =`⏎`  { …
        // }`⏎`  member …` layout — the record/union/enum arms leave the separator
        // at the cursor) or directly at the cursor (the inline `type R = { … }
        // member …`, whose separator the `}`-on-own-line close absorbed). Parse
        // them via [`Self::parse_sig_member_block_items`] while the `TYPE_DEFN` is
        // the open node, so they become direct `MEMBER_SIG` children *after* the
        // repr node (the outer slot — not wrapped in an `OBJECT_MODEL_REPR`, which
        // is for a member-*first* body's own members). The FCS-invalid layout
        // `type R = { … }`⏎`  member …` (inline record, indented member) puts the
        // body-close `OBLOCKEND` *before* the member, so neither gate fires and it
        // is left for the close handling / module loop to flag, matching FCS.
        //
        // Gated on `opened_block` (mirroring the slice-3a object-model gate): a
        // real offside body block was opened (`type R =` opens one after `=`),
        // inside which members and the closing `OBLOCKEND` live. The sole blockless
        // `=` body is the invalid column-0 after-keyword-attribute regime
        // (`type [<A>]`⏎`T = int`⏎`val y …`), where the dedented sibling `val y`
        // arrives as `OBLOCKSEP`/`Val` with no body block — without this gate it
        // would be swallowed as a phantom member, losing the top-level export. FCS
        // rejects that whole regime; leaving the `val` for the module loop keeps it
        // a sibling (and the regime is flagged elsewhere).
        let at_sep_then_item = opened_block
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            )
            && self.sig_member_item_follows_block_sep();
        if at_sep_then_item {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), ""); // the OBLOCKSEP
            self.pos += 1;
        }
        let parsed_bare_members =
            at_sep_then_item || (opened_block && self.peek_is_sig_member_block_start());
        if parsed_bare_members {
            // A delegate takes no augmentation
            // (`parsAugmentationsIllegalOnDelegateType`); a trailing bare-member run
            // on one is flagged (mirroring the impl) and still parsed (lossless).
            let aug_span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.parse_sig_member_block_items();
            if is_delegate {
                self.errors.push(ParseError {
                    message: DELEGATE_AUGMENTATION_ERROR.to_string(),
                    span: aug_span,
                });
            }
        }

        // Whether the body's offside block opened and closed — the `and`-chain
        // gate (an inline `type A = int and B = …` leaves the block open, so the
        // `and` is not chained). A single-line (blockless) body never opened a
        // block, so it stays `false`, matching the impl `parse_type_defn_repr`.
        let mut closed_block = false;
        if opened_block {
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
            ) {
                // Closing `OBLOCKEND` — consume as a zero-width ERROR advancing
                // only `pos` (the `parse_type_defn_repr` discipline): a following
                // swallowed `type`/`and` keyword sharing its byte span survives.
                self.builder
                    .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                self.pos += 1;
                closed_block = true;
            } else {
                // A leftover the bare-member loop did not model (a nested-type sig,
                // or an attributed `inherit`/`interface`).
                // `skip_offside_block_interior_as_error` depth-tracks to and
                // consumes the matching `OBLOCKEND`, so the body block *is* closed
                // afterwards — report `closed_block = true` so an `and`-continuation
                // on the next line is still folded into this group, rather than
                // spilling its body to the enclosing loop where members would leak
                // as phantom top-level `val`s.
                self.push_type_sig_unsupported_body_error();
                self.skip_offside_block_interior_as_error();
                closed_block = true;
            }
        }
        // A trailing `with`-augmentation (slice 6) can follow the repr at the
        // cursor: after a *single-line* (blockless) body, or after the body block
        // closed — LexFilter grants `with` undentation grace, so the `OBLOCKEND`
        // is consumed above and the raw `with` then sits here. Two LexFilter forms,
        // exactly as the slice-4 bodyless augmentation:
        //  * `Raw(with) OBLOCKBEGIN … OBLOCKEND ODECLEND` — supported member-sig
        //    starts. Emit the `WITH_TOK` and parse the member sigs into the outer
        //    slot via [`Self::parse_with_augmentation_members`] (`sig = true`),
        //    which drains the augment's `OBLOCKEND` + `ODECLEND`.
        //  * `OWITH … OEND` ([`Virtual::With`]) — a same-line `[<A>] member` /
        //    `private member` first member, a later slice (FCS errors), contained
        //    as ERROR via `skip_owith_block_as_error`.
        // Either way the augment drains to its close, so a following newline `and`
        // continuation stays contained in this group (mark the body closed).
        //
        // A trailing `with` is illegal on a delegate
        // (`parsAugmentationsIllegalOnDelegateType`), and a `with` *after* a bare
        // trailing-member run is FCS's `checkForMultipleAugmentations` ("At most one
        // 'with' augmentation is permitted"). Flag whichever applies (mirroring the
        // impl `parse_type_defn_repr`) and still parse the block (lossless; FCS
        // drops the whole declaration).
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::With)), sp)) => {
                let span = sp.clone();
                if is_delegate {
                    self.errors.push(ParseError {
                        message: DELEGATE_AUGMENTATION_ERROR.to_string(),
                        span,
                    });
                } else if parsed_bare_members {
                    self.errors.push(ParseError {
                        message: "At most one 'with' augmentation is permitted".to_string(),
                        span,
                    });
                }
                self.bump_into(SyntaxKind::WITH_TOK);
                self.parse_with_augmentation_members(true, true);
                closed_block = true;
            }
            Some((Ok(FilteredToken::Virtual(Virtual::With)), sp)) => {
                let span = sp.clone();
                self.errors.push(ParseError {
                    message: "this member signature in a `with` augmentation is not yet \
                              supported (later phase-10 slice)"
                        .to_string(),
                    span,
                });
                self.skip_owith_block_as_error();
                closed_block = true;
            }
            _ => {}
        }
        closed_block
    }

    /// `true` iff the cursor is at the start of a signature member sig parsed by
    /// [`Self::parse_member_sig`] — a `member`, `abstract [member]`,
    /// `static member`, or `static abstract [member]` keyword run (slice 3a), or a
    /// `new` constructor sig (slice 3e), optionally preceded by a leading
    /// accessibility modifier on the `new` form (slice V2). A leading `static`
    /// counts only when a `member` or `abstract` follows — `static val` / `static
    /// type` (a static field / nested type) are later slices.
    fn peek_is_member_sig_start(&self) -> bool {
        match self.peek() {
            Some((
                Ok(FilteredToken::Raw(
                    Token::Abstract | Token::Member | Token::New | Token::Override | Token::Default,
                )),
                _,
            )) => true,
            Some((Ok(FilteredToken::Raw(Token::Static)), _)) => matches!(
                self.nth_significant_raw_at_pos(1),
                Some(Token::Member | Token::Abstract)
            ),
            // A leading accessibility modifier starts a member sig only when a `new`
            // ctor follows (slice V2): FCS accepts `opt_access NEW` (`pars.fsy:1040`)
            // but rejects a leading modifier before any other member keyword (it must
            // sit before the name there — slice V1 / deferred).
            Some((Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)), _)) => {
                matches!(self.nth_significant_raw_at_pos(1), Some(Token::New))
            }
            _ => false,
        }
    }

    /// `true` iff the cursor is at the `(` of an SRTP member constraint
    /// `^T : (static member M : sig)` whose member signature this slice handles:
    /// a member-keyword run (`static`/`abstract`/`member`, plus name-position
    /// accessibility) followed by a plain identifier or an operator name
    /// (`(static member (+) : …)`), or a `new` constructor signature
    /// (`(new : unit -> ^T)`). A member constraint is the only parenthesised form
    /// valid after a constraint `:`, so a non-matching `^T : (…)` stays on the
    /// error path.
    pub(super) fn paren_opens_member_sig_constraint(&self) -> bool {
        let mut sig = self.significant_raw_from_cursor();
        sig.next(); // the `(`
        Self::member_sig_body_is_supported(&mut sig)
    }

    /// Shared `classMemberSpfn` introducer-and-name check used by both the SRTP
    /// member *constraint* gate ([`Self::paren_opens_member_sig_constraint`]) and
    /// the trait-call expression gate ([`Parser::at_trait_call_body`]). `sig` must
    /// be positioned *just past* the member-sig's opening `(` (the caller consumes
    /// it). Returns `true` only for the name shapes [`Self::parse_member_sig`]
    /// parses faithfully, so an unsupported form stays on the error path rather
    /// than committing and misparsing.
    pub(super) fn member_sig_body_is_supported<'a>(
        sig: &mut impl Iterator<Item = &'a Token<'src>>,
    ) -> bool
    where
        'src: 'a,
    {
        let mut tok = sig.next();
        // A leading accessibility modifier is valid (FCS's `opt_access`) only
        // before a `new` ctor; before any other introducer it is a malformed
        // member sig, so require `new` to follow it.
        if matches!(tok, Some(Token::Internal | Token::Private | Token::Public)) {
            return matches!(sig.next(), Some(Token::New));
        }
        // A `new` constructor constraint — `parse_member_sig` reads the `new`
        // keyword as the name, so no trailing ident is required.
        if matches!(tok, Some(Token::New)) {
            return true;
        }
        // A member introducer run. FCS's `classMemberSpfn` requires a
        // `memberSpecFlags` token (`static` / `abstract` / `member`), so at least
        // one must appear — a bare `(Zero : …)` (no introducer) stays on the
        // error path. `static` alone *is* an introducer here: in a constraint
        // there is no `static let`/`val`/`do` to disambiguate, and FCS accepts
        // `(static Zero : ^T)` as a `Static`-keyword member sig.
        let mut saw_introducer = false;
        if matches!(tok, Some(Token::Static)) {
            saw_introducer = true;
            tok = sig.next();
        }
        if matches!(tok, Some(Token::Abstract)) {
            saw_introducer = true;
            tok = sig.next();
        }
        if matches!(tok, Some(Token::Member)) {
            saw_introducer = true;
            tok = sig.next();
        }
        if !saw_introducer {
            return false;
        }
        // An optional name-position `inline` modifier (FCS's `opt_inline` before
        // `opt_access`) — mirrors the consume in [`Parser::parse_member_sig`], so
        // the gate and the parser stay in lockstep and `(static member inline M :
        // ^a -> int) x` / `^T : (static member inline Zero : ^T)` reach the parser
        // rather than being rejected here.
        if matches!(tok, Some(Token::Inline)) {
            tok = sig.next();
        }
        // An optional name-position accessibility modifier (`member private M`),
        // then the name.
        if matches!(tok, Some(Token::Internal | Token::Private | Token::Public)) {
            tok = sig.next();
        }
        // The name is a plain `identifier`, an operator name, or an
        // active-pattern name that [`Parser::parse_member_sig`] reads via the
        // binding-head machinery — the glued `(*)` ([`Token::LParenStarRParen`]),
        // a general `( op )`, or an active-pattern name `( | … )`. This must
        // mirror [`Parser::peek_operator_head`] / [`Parser::at_paren_op_value_pat`]
        // / [`Parser::at_active_pat_name`] *exactly* (same operator set, incl. the
        // spaced `( * )` the second admits via `allow_star`; the active pattern
        // keyed on a leading bar like the third), so the gate and the parser stay
        // in lockstep — an over-accept routes an unparseable form into
        // `parse_member_sig`, an under-accept leaves a valid one on the error path.
        match tok {
            Some(Token::Ident(_) | Token::QuotedIdent(_)) => true,
            Some(Token::LParenStarRParen) => true,
            Some(Token::LParen) => match sig.next() {
                // Active-pattern name (`(|Foo|_|)`): `(` then a bare `|`.
                Some(Token::Bar) => true,
                // Operator name (`(+)`, `( * )`): `( op )`.
                Some(t) if is_paren_operator_name(t) || matches!(t, Token::Op("*")) => {
                    matches!(sig.next(), Some(Token::RParen))
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// `true` iff the cursor starts an *unattributed* signature member-block item
    /// modelled so far — a `member`/`abstract`/`static member` sig
    /// ([`Self::peek_is_member_sig_start`], slice 3a) or an `inherit` / `interface`
    /// / `val`-field member sig (slice 3b). Drives both the body dispatch
    /// ([`Self::parse_sig_type_defn_repr`]) and the inter-member separator gate.
    ///
    /// A leading `[<…>]` is **not** claimed: an attributed member sig is a later
    /// slice. `classify_object_model_item` looks *through* an attribute run, so it
    /// would otherwise report the underlying item while the cursor is still on the
    /// `[<` — leaving the reused member parsers to misread the attribute opener.
    pub(super) fn peek_is_sig_member_block_start(&self) -> bool {
        // An attributed member sig (slice 8): look through a leading `[<…>]`
        // attribute run to the introducer in the *same* offside scope.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) {
            return self.attributed_member_sig_follows_from(self.pos);
        }
        self.peek_is_member_sig_start()
            || matches!(
                self.classify_object_model_item(),
                Some(
                    ObjectModelItem::ValField
                        | ObjectModelItem::Inherit
                        | ObjectModelItem::Interface
                )
            )
    }

    /// `true` iff the filtered stream from index `from` (inclusive) is a leading
    /// `[<…>]` attribute run followed — in the *same* offside scope — by a
    /// member-sig introducer (`member`/`abstract`/`static`/`val`/`new`/`inherit`/
    /// `interface`, or an accessibility modifier before `new`). Slice 8: the gate
    /// for an *attributed* member sig.
    ///
    /// The walk works on the *filtered* stream (not the raw one) so it respects
    /// offside boundaries: only a `BlockSep` (an offside continuation —
    /// `[<A>]`⏎`member …`) or another attribute list may sit between the run and
    /// the introducer. A `BlockEnd` (or any non-introducer) means the attribute is
    /// *dangling* — its block has closed and the following spec is a dedented
    /// sibling, not a member (`type T =`⏎`  [<A>]`⏎`val y`, which FCS rejects) — so
    /// return `false` and let the caller flag/contain it rather than swallow the
    /// sibling as a phantom member.
    fn attributed_member_sig_follows_from(&self, from: usize) -> bool {
        let toks = &self.filtered_tokens;
        let mut i = from;
        let skip_trivia = |mut i: usize| {
            while i < toks.len()
                && matches!(&toks[i].0, Ok(FilteredToken::Raw(t)) if trivia_kind(t).is_some())
            {
                i += 1;
            }
            i
        };
        loop {
            i = skip_trivia(i);
            let Some((Ok(ft), _)) = toks.get(i) else {
                return false;
            };
            match ft {
                // Consume one attribute list `[< … >]`, then loop (another list, an
                // offside `BlockSep`, or the introducer may follow).
                FilteredToken::Raw(Token::LBrackLess) => {
                    i += 1;
                    while i < toks.len()
                        && !matches!(&toks[i].0, Ok(FilteredToken::Raw(Token::GreaterRBrack)))
                    {
                        i += 1;
                    }
                    if i >= toks.len() {
                        return false;
                    }
                    i += 1; // past `>]`
                }
                // An offside continuation to the next list / the introducer.
                FilteredToken::Virtual(Virtual::BlockSep) => i += 1,
                // `static` is a two-token introducer *prefix* (`static member` /
                // `static abstract` / `static val`): require its continuation in the
                // *same* scope. Returning true on a bare `static` whose real head
                // (`val`/`member`) sits across an `OBLOCKEND` would let the downstream
                // raw classifier read `static val` past the block close and swallow a
                // dedented sibling. Consume `static` and loop — the next significant
                // token must then be `member`/`abstract`/`val` (else the `_` /
                // `BlockEnd` arm returns false).
                FilteredToken::Raw(Token::Static) => i += 1,
                // The introducer — a supported member-sig start. `interface` in
                // member position is LexFilter-relabelled to
                // `Virtual::InterfaceMember`, so accept that virtual too.
                FilteredToken::Raw(
                    Token::Member
                    | Token::Abstract
                    | Token::New
                    | Token::Val
                    | Token::Inherit
                    | Token::Interface
                    | Token::Override
                    | Token::Default,
                )
                | FilteredToken::Virtual(Virtual::InterfaceMember) => return true,
                // A leading accessibility modifier is valid only before `new`
                // (`[<A>] private new : …`, FCS's `opt_attributes opt_access NEW`).
                FilteredToken::Raw(Token::Internal | Token::Private | Token::Public) => {
                    let j = skip_trivia(i + 1);
                    return matches!(toks.get(j), Some((Ok(FilteredToken::Raw(Token::New)), _)));
                }
                // A `BlockEnd` (dangling attribute) or anything else: not an
                // attributed member sig in this scope.
                _ => return false,
            }
        }
    }

    /// `[<…>]` attribute run followed — in the *same* offside scope — by a bare
    /// `val`. Unlike [`Self::attributed_member_sig_follows_from`] (which admits
    /// every member introducer for the *in-body* attributed-member case), only
    /// `val` qualifies: after a bodyless opaque type, FCS promotes an abutting
    /// attributed `val` (`type Shape`⏎`  [<A>] val X : …`) to a module-level
    /// `SynModuleSigDecl.Val`, but rejects an attributed *non*-`val`
    /// (`[<A>] type`/`member`/… — not a valid module-sig-decl). Walks the
    /// *filtered* stream so offside boundaries are respected: only another
    /// attribute list or a `BlockSep` may sit between the run and the `val`.
    fn attributed_val_follows_from(&self, from: usize) -> bool {
        let toks = &self.filtered_tokens;
        let mut i = from;
        let skip_trivia = |mut i: usize| {
            while i < toks.len()
                && matches!(&toks[i].0, Ok(FilteredToken::Raw(t)) if trivia_kind(t).is_some())
            {
                i += 1;
            }
            i
        };
        loop {
            i = skip_trivia(i);
            let Some((Ok(ft), _)) = toks.get(i) else {
                return false;
            };
            match ft {
                // Consume one attribute list `[< … >]`, then loop.
                FilteredToken::Raw(Token::LBrackLess) => {
                    i += 1;
                    while i < toks.len()
                        && !matches!(&toks[i].0, Ok(FilteredToken::Raw(Token::GreaterRBrack)))
                    {
                        i += 1;
                    }
                    if i >= toks.len() {
                        return false;
                    }
                    i += 1; // past `>]`
                }
                // An offside continuation to the next list / the `val`.
                FilteredToken::Virtual(Virtual::BlockSep) => i += 1,
                // Only a bare `val` promotes to a module-level decl.
                FilteredToken::Raw(Token::Val) => return true,
                _ => return false,
            }
        }
    }

    /// Parse a signature object-model body of member signatures (phase 10.14)
    /// into a [`SyntaxKind::OBJECT_MODEL_REPR`] — FCS's
    /// `SynTypeDefnSigRepr.ObjectModel(…, memberSigs, …)`. The caller
    /// ([`Self::parse_sig_type_defn_repr`]) has consumed the body-opening
    /// `OBLOCKBEGIN`, so the cursor sits on the first member keyword. Each item is
    /// parsed by the matching parser — `member`/`abstract`/`static member` →
    /// [`Self::parse_member_sig`] (slice 3a); `val`-field → [`Self::parse_val_field_at`],
    /// `inherit` → [`Self::parse_sig_inherit_member`], `interface` →
    /// [`Self::parse_interface_member`] (slice 3b, reusing the impl member nodes).
    /// Items are separated by an `OBLOCKSEP` (the offside layout) and/or `;`/`;;`
    /// (FCS's `opt_seps`). A member-sig kind not yet modelled (a nested-type sig,
    /// or an attributed `inherit`/`interface`) ends
    /// the loop and is left to the caller's trailing-body handling (skipped with a
    /// diagnostic). The body-closing `OBLOCKEND` is likewise left for the caller.
    fn parse_sig_object_model_repr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::OBJECT_MODEL_REPR));
        self.parse_sig_member_block_items();
        self.builder.finish_node(); // OBJECT_MODEL_REPR
    }

    /// Parse the member-signature items of a signature object-model body (shared
    /// by the lightweight [`Self::parse_sig_object_model_repr`] and the
    /// explicit-kind [`Self::parse_sig_kind_marked_repr`]). Emits the member
    /// nodes directly into the open repr node (no node of its own). Each
    /// `member`/`abstract`/`static member` / `val`-field / `inherit` / `interface`
    /// sig is parsed by its matching parser; items are separated by an `OBLOCKSEP`
    /// (only consumed when another member follows — preserving a dedented sibling
    /// boundary) and/or `;`/`;;`. The loop stops at anything else (a body-closing
    /// `OBLOCKEND`, the explicit `end` keyword, a module-separator `OBLOCKSEP`, or
    /// a deferred member kind), leaving it for the caller.
    pub(super) fn parse_sig_member_block_items(&mut self) {
        loop {
            // Leading attribute lists on a member sig (slice 8). In member position
            // a leading `[<` can only be an attributed member sig, so parse the
            // lists under a checkpoint and thread it into the member node (opened at
            // `cp`, so the attributes become its leading children — FCS homes them
            // in `SynValSig.attributes` / `SynField.attributes`). Mirrors the
            // impl-side `parse_member_block_items`. The offside `[<A>]⏎member …`
            // layout leaves a `BlockSep` before the (real-token) member keyword;
            // skip it.
            let member_cp = if let Some((Ok(FilteredToken::Raw(Token::LBrackLess)), span)) =
                self.peek().cloned()
            {
                self.drain_raw_up_to(span.start);
                let cp = self.builder.checkpoint();
                self.parse_attribute_lists();
                // `member`/`abstract`/`val`/… are real filtered tokens (not
                // LexFilter-swallowed), so a plain `bump_into` is safe — no swallowed
                // keyword shares the `BlockSep`'s span here.
                while matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                ) {
                    self.bump_into(SyntaxKind::ERROR);
                }
                Some(cp)
            } else {
                None
            };

            if self.peek_is_member_sig_start() {
                // A type-definition member-sig body admits a `= <literal>` value
                // (phase 10.12 member-literal): `allow_literal = true`.
                let had_literal_rhs = self.parse_member_sig_at(member_cp, true);
                // A `= <literal>` member value (phase 10.12 member-literal) leaves
                // its offside RHS-block close `OBLOCKEND` then the item's
                // `ODECLEND` (mirrors the impl loop's `has_rhs_block` terminator);
                // drain both so the next member sig — or the body-close — is
                // reached rather than the loop breaking on the RHS-close virtual.
                // (A trailing `;` is absorbed by the RHS seq-block, as at module
                // level; a newline-separated sibling's `OBLOCKSEP` is handled by
                // the separator arm below on the next iteration.)
                if had_literal_rhs {
                    if matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
                    ) {
                        self.bump_into(SyntaxKind::ERROR);
                    }
                    if matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::DeclEnd)), _))
                    ) {
                        self.bump_into(SyntaxKind::ERROR);
                    }
                }
                continue;
            }
            // `val`-field / `inherit` / `interface` member sigs (slice 3b) reuse
            // the impl-side member nodes (the sig FCS union differs but the
            // normalised form is shared). A sig `val`-field has no `= <expr>` RHS
            // and a sig `interface` has no `with`-block, so the no-body / no-with
            // paths of the reused `parse_val_field_at` / `parse_interface_member`
            // apply; `inherit` uses a sig-specific parser (an app type, no ctor
            // args). An attributed `val`-field threads the checkpoint (slice 8);
            // attributed `inherit`/`interface` member sigs are a later slice — flag
            // and leave the parsed `ATTRIBUTE_LIST`s as bare siblings, then parse
            // the item itself so the rest of the block still parses.
            match self.classify_object_model_item() {
                Some(ObjectModelItem::ValField) => {
                    self.parse_val_field_at(member_cp);
                    continue;
                }
                Some(ObjectModelItem::Inherit) => {
                    if member_cp.is_some() {
                        self.push_attributed_member_sig_unsupported_error();
                    }
                    self.parse_sig_inherit_member();
                    continue;
                }
                Some(ObjectModelItem::Interface) => {
                    if member_cp.is_some() {
                        self.push_attributed_member_sig_unsupported_error();
                    }
                    self.parse_interface_member(true);
                    continue;
                }
                _ => {}
            }
            // A leading `[<…>]` run with no member sig following it — a *dangling*
            // in-body attribute (FCS rejects it). Flag it (so it is not silently
            // accepted — e.g. inside an explicit `class … end` body whose caller
            // would otherwise reach `end` cleanly) and break so the caller's
            // trailing-body handling contains it (the attribute lists stay bare
            // children); without the break the loop would spin on the same cursor.
            if member_cp.is_some() {
                self.push_attributed_member_sig_unsupported_error();
                break;
            }
            match self.peek() {
                // An inter-member offside separator (`OBLOCKSEP`) — but **only**
                // when another member-block item follows it. In the block layout
                // (`type I =`⏎`  member M`⏎`  member N`) members sit inside a
                // member-body block separated by `OBLOCKSEP` (the body closes with
                // a distinct `OBLOCKEND`). In the *blockless* column-0 attribute
                // regime the type opens no body block, so an `OBLOCKSEP` here is
                // instead the *module* separator before a dedented sibling spec —
                // consuming it would erase the boundary and the sibling would be
                // skipped/leaked. So peek past the separator: consume it
                // (zero-width) only if a member-block item follows; otherwise leave
                // it and break.
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                    if self.sig_member_item_follows_block_sep() =>
                {
                    self.builder
                        .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
                    self.pos += 1;
                }
                // A `;` / `;;` top separator (FCS's `opt_seps`) between member
                // sigs on one line (`abstract A : int; abstract B : int`).
                Some((Ok(FilteredToken::Raw(Token::Semi)), _)) => {
                    self.bump_into(SyntaxKind::SEMI_TOK);
                }
                Some((Ok(FilteredToken::Raw(Token::SemiSemi)), _)) => {
                    self.bump_into(SyntaxKind::SEMISEMI_TOK);
                }
                // Anything else (the body close `OBLOCKEND`, the explicit `end`
                // keyword, a module-separator `OBLOCKSEP` before a dedented
                // sibling, or a deferred member kind) ends the member run; the
                // caller's trailing-body handling and the enclosing loop take over.
                _ => break,
            }
        }
    }

    /// `true` iff the repr at the cursor is an explicit-`end`-delimited
    /// object-model body: a `class`/`struct`/`interface` kind marker (the shared
    /// [`Self::peek_is_kind_marked_repr_start`]) **or** the verbose `begin … end`
    /// body (a sig-only spelling of the *unspecified*-kind object model). Routed
    /// through [`Self::parse_sig_kind_marked_repr`]. `begin` heads no *type* form
    /// (unlike `struct (…)`), so it needs no disambiguation lookahead.
    fn peek_is_sig_kind_marked_repr_start(&self) -> bool {
        self.peek_is_kind_marked_repr_start()
            || matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Begin)), _)))
    }

    /// Parse an explicit-kind-marked signature object-model body —
    /// `class … end` / `struct … end` / `interface … end` (phase 10.14 slice
    /// 3c), or the verbose `begin … end` body (an *unspecified*-kind object
    /// model) — into a [`SyntaxKind::OBJECT_MODEL_REPR`], FCS's
    /// `SynTypeDefnSigRepr.ObjectModel(Class|Struct|Interface|Unspecified, memberSigs, _)`.
    /// Mirrors the impl-side [`Self::parse_kind_marked_repr`] framing (the kind
    /// keyword marker, the inner member block, the closing `end`), but parses
    /// member *signatures* via [`Self::parse_sig_member_block_items`] rather than
    /// member definitions. The caller has consumed any body-opening `OBLOCKBEGIN`,
    /// so the cursor sits on the `class`/`struct`/`interface` keyword; the outer
    /// block's closing `OBLOCKEND` (if any) is left for the caller.
    ///
    /// Unlike a bare member body, the explicit `end` delimits the body, so there
    /// is no blockless column-0 ambiguity — this needs no `opened_block` gate.
    fn parse_sig_kind_marked_repr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::OBJECT_MODEL_REPR));
        // The kind keyword → the marker token the facade reads the kind off. A
        // verbose `begin … end` body carries *no* kind (FCS's
        // `SynTypeDefnKind.Unspecified`, `begin`/`end` pure delimiters), so it
        // rides a `BEGIN_TOK` marker — which no `is_class`/`is_struct`/
        // `is_interface` accessor reads, leaving the projected kind `Unspecified`,
        // exactly as a bare `type T = member …` body.
        let kw = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Class)), _)) => SyntaxKind::CLASS_TOK,
            Some((Ok(FilteredToken::Raw(Token::Struct)), _)) => SyntaxKind::STRUCT_TOK,
            Some((Ok(FilteredToken::Raw(Token::Begin)), _)) => SyntaxKind::BEGIN_TOK,
            _ => SyntaxKind::INTERFACE_TOK,
        };
        self.bump_into(kw);
        // The inner member-block `OBLOCKBEGIN` (`class`/`interface` are FCS
        // `AddBlockEnd::Yes`; `struct` opens none) — consume one if present.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
        // The member sigs; the loop stops at the raw `end` or at the first member
        // kind it does not model.
        self.parse_sig_member_block_items();
        // Everything between the modelled members and the closing `end` — the
        // inner block's close `OBLOCKEND`/`OBLOCKSEP` run (`class`/`interface`
        // emit one before `end`; `struct` none) **and** any unsupported trailing
        // member sig the loop stopped on (a `new`-ctor / property get-set /
        // attributed member, slice 3d/3e) — is drained as `ERROR` up to `end`.
        // Without this the unsupported item + `end` would leak to the enclosing
        // loop as top-level garbage (the inline `type T = class … end` body is
        // blockless, so the caller's trailing-body skip does not run). A *raw*
        // token here (not a layout virtual) is unsupported member content, so it
        // earns one diagnostic; the bare close virtuals do not.
        //
        // A skipped unsupported member may itself contain a nested `… end`
        // (a deferred `static type N = class … end` member sig), so track the
        // depth of nested `class`/`struct`/`interface`/`begin` openers and break
        // only on the *matching* outer `end` — otherwise the nested `end` would be
        // mistaken for this body's closer and the rest would leak. `struct` is
        // gated exactly like [`Self::peek_is_kind_marked_repr_start`]: a
        // `struct (…)` tuple / `struct {| … |}` anon-record is a *type*, not an
        // `end`-delimited body (e.g. `new : struct (int * int) -> T`), so it must
        // not bump the depth.
        let mut unsupported_member = false;
        let mut nested_end_depth = 0u32;
        while let Some((res, _)) = self.peek() {
            let opens_nested_end = match self.peek() {
                Some((
                    Ok(FilteredToken::Raw(Token::Class | Token::Interface | Token::Begin)),
                    _,
                )) => true,
                Some((Ok(FilteredToken::Raw(Token::Struct)), _)) => !matches!(
                    self.nth_significant_raw_at_pos(1),
                    Some(Token::LParen | Token::LBraceBar)
                ),
                _ => false,
            };
            match res {
                Ok(FilteredToken::Raw(Token::End)) if nested_end_depth == 0 => break,
                Ok(FilteredToken::Raw(Token::End)) => {
                    nested_end_depth -= 1;
                    unsupported_member = true;
                    self.bump_into(SyntaxKind::ERROR);
                }
                _ if opens_nested_end => {
                    nested_end_depth += 1;
                    unsupported_member = true;
                    self.bump_into(SyntaxKind::ERROR);
                }
                Ok(FilteredToken::Virtual(_)) => self.bump_into(SyntaxKind::ERROR),
                _ => {
                    unsupported_member = true;
                    self.bump_into(SyntaxKind::ERROR);
                }
            }
        }
        if unsupported_member {
            self.push_type_sig_unsupported_body_error();
        }
        // The explicit `end` closer.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::End)), _))) {
            self.bump_into(SyntaxKind::END_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `end` to close the type definition".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // OBJECT_MODEL_REPR
    }

    /// `true` iff a signature member-block item starts immediately after the
    /// `OBLOCKSEP` at the cursor — the look-past-separator gate for
    /// [`Self::parse_sig_object_model_repr`]. `member`/`abstract`/`static` /
    /// `inherit`/`val` / `new` are real raw tokens; a member-position `interface`
    /// is the relabelled [`Virtual::InterfaceMember`]. A leading accessibility
    /// modifier counts only when it precedes a `new` ctor (slice V2) — so the gate
    /// looks one filtered token further, to the keyword after the modifier.
    fn sig_member_item_follows_block_sep(&self) -> bool {
        let next = self.next_non_trivia_filtered_after_pos();
        if matches!(
            next,
            Some(FilteredToken::Raw(
                Token::Member
                    | Token::Abstract
                    | Token::Static
                    | Token::Inherit
                    | Token::Val
                    | Token::New
                    | Token::Override
                    | Token::Default
            )) | Some(FilteredToken::Virtual(Virtual::InterfaceMember))
        ) {
            return true;
        }
        // A leading `[<…>]` after the separator opens an attributed member sig
        // (slice 8) — but only when the attribute run leads to an introducer in the
        // *same* offside scope (not a dangling `[<A>]` whose block closes before a
        // dedented sibling). Reuse the scope-aware lookahead.
        if matches!(next, Some(FilteredToken::Raw(Token::LBrackLess)))
            && let Some(idx) = self.next_non_trivia_filtered_index_after(self.pos)
        {
            return self.attributed_member_sig_follows_from(idx);
        }
        if matches!(
            next,
            Some(FilteredToken::Raw(
                Token::Internal | Token::Private | Token::Public
            ))
        ) && let Some(idx) = self.next_non_trivia_filtered_index_after(self.pos)
        {
            return matches!(
                self.next_non_trivia_filtered_after_index(idx),
                Some(FilteredToken::Raw(Token::New))
            );
        }
        false
    }

    /// Parse one signature member sig (`SynMemberSig.Member`) into a
    /// [`SyntaxKind::MEMBER_SIG`] node — `[STATIC_TOK?, ABSTRACT_TOK?, MEMBER_TOK?,
    /// VAL_SIG]`, where the [`VAL_SIG`](SyntaxKind::VAL_SIG) child carries the name
    /// and `: <type>` (the same carrier as a `val` sig / abstract slot) and the
    /// leading keyword tokens select the member kind. The signature type routes
    /// through [`Self::parse_type_with_constraints`] (FCS's
    /// `topTypeWithTypeConstraints`, `pars.fsy:969`), so a trailing `when` clause
    /// (`'T -> 'T when 'T : comparison` → `SynType.WithGlobalConstraints`) folds
    /// in; member-level explicit type parameters, accessibility/`inline`
    /// modifiers, and a trailing `with get[, set]` property clause are later
    /// slices (they stop cleanly at the name/`:`). Caller has verified the cursor
    /// is at a member-sig start ([`Self::peek_is_member_sig_start`]).
    pub(super) fn parse_member_sig(&mut self) {
        // The SRTP member-constraint context (`^T : (member M : sig)`) has no
        // `= <literal>` form — and its `)` is LexFilter-swallowed, so the next
        // filtered token after the sig type is the enclosing binding's `=`, which
        // must not be claimed as a member literal: pass `allow_literal = false`.
        let _ = self.parse_member_sig_at(None, false);
    }

    /// As [`Self::parse_member_sig`], but with `cp = Some(checkpoint)` the caller
    /// has already emitted one or more leading `ATTRIBUTE_LIST`s (the attributed
    /// member sig `[<CLIEvent>] abstract M : int`, slice 8) after the checkpoint;
    /// the `MEMBER_SIG` is opened *at* that checkpoint so the attribute lists become
    /// its leading children (FCS homes them in `SynValSig.attributes`, read back by
    /// [`MemberSig::attributes`](crate::syntax::MemberSig::attributes)). `None` is
    /// the plain (unattributed) form.
    ///
    /// `allow_literal` gates the trailing `= <literal>` value (phase 10.12
    /// member-literal): `true` in a type-definition member-sig body (`type X =`⏎`
    /// member a : int = 10`), `false` in the SRTP member-constraint context
    /// (`^T : (static member Zero : ^T)`), where the constraint's `)` is
    /// LexFilter-swallowed so the *next* filtered token after the type is the
    /// enclosing binding's `=` — which must not be mistaken for a member literal.
    ///
    /// Returns `true` iff a `= <literal>` value was consumed — its offside RHS
    /// block leaves a close `OBLOCKEND` the caller's member-block loop must drain
    /// (mirrors the impl-side `has_rhs_block`).
    pub(super) fn parse_member_sig_at(
        &mut self,
        cp: Option<rowan::Checkpoint>,
        allow_literal: bool,
    ) -> bool {
        match cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEMBER_SIG)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEMBER_SIG)),
        }
        // A *leading* accessibility modifier (slice V2) — FCS accepts `opt_access`
        // before the keyword only on the `new` ctor (`pars.fsy:1040`,
        // `opt_access NEW COLON …`), where it is the ctor's visibility. Consume it
        // as an `ACCESS_TOK` (a `MEMBER_SIG`-level token, before the `new` keyword);
        // the normaliser elides accessibility. Gated on `new` following so it is
        // never consumed before a `member`/`abstract`/`val` keyword (those take the
        // modifier before the name — slice V1 — or reject a leading one).
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Internal | Token::Private | Token::Public
                )),
                _,
            ))
        ) && matches!(self.nth_significant_raw_at_pos(1), Some(Token::New))
        {
            self.bump_into(SyntaxKind::ACCESS_TOK);
        }
        // A `new : <type>` constructor sig (slice 3e) — FCS's
        // `SynMemberSig.Member(valSig ident="new", CtorMemberFlags)`, leading
        // keyword `New`. The `new` keyword *is* the name (there is no separate
        // `IDENT`); it is emitted as `NEW_TOK` and the normaliser reads the name
        // "new" from the leading keyword. Handled up front because it takes no
        // `member`/`abstract`/`static` prefix and no `IDENT`.
        let is_new_ctor = matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::New)), _)));
        let mut is_abstract = false;
        if is_new_ctor {
            self.bump_into(SyntaxKind::NEW_TOK);
        } else if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Override)), _))
        ) {
            // `override M : …` / `default M : …` — standalone leading keywords,
            // FCS's `SynLeadingKeyword.Override`/`.Default`. They are the whole
            // `memberSpecFlags` on their own: FCS's `classMemberSpfn` picks
            // *either* `abstractMemberFlags` *or* `memberFlags`, so a combined
            // `abstract override` / `static default` is not a legal introducer
            // run. Handling them as an exclusive arm (not after the
            // `static`/`abstract`/`member` run) keeps those combinations on the
            // error path — the keyword is not consumed there, so the name parse
            // trips on it, matching FCS's rejection. The keyword *is* the
            // member-kind marker; the name follows as an ordinary `IDENT`.
            self.bump_into(SyntaxKind::OVERRIDE_TOK);
        } else if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Default)), _))
        ) {
            self.bump_into(SyntaxKind::DEFAULT_TOK);
        } else {
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Static)), _))
            ) {
                self.bump_into(SyntaxKind::STATIC_TOK);
            }
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Abstract)), _))
            ) {
                self.bump_into(SyntaxKind::ABSTRACT_TOK);
                is_abstract = true;
            }
            if matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Member)), _))
            ) {
                self.bump_into(SyntaxKind::MEMBER_TOK);
            }
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::VAL_SIG));
        // A name-position `inline` modifier — FCS's `classMemberSpfn` `opt_inline`
        // before `opt_access` (`[static] member [inline] opt_access nameop`,
        // `pars.fsy:969`), setting `SynValSig.isInline`. The flag is elided by the
        // normaliser, so consuming the `INLINE_TOK` is all that is needed for the
        // shape to match (`member inline Bind : …`, the FSharp.Core builder sigs).
        // A `new`-ctor takes no `inline`. (`inline` is parse-accepted on an
        // `abstract` member too — the abstract-slot path is likewise lenient — and
        // rejected only in a later phase, so it is not flagged here.)
        if !is_new_ctor
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Inline)), _))
            )
        {
            self.bump_into(SyntaxKind::INLINE_TOK);
        }
        // A name-position accessibility modifier (slice V1) — FCS's `classMemberSpfn`
        // `opt_access` before the name (`pars.fsy:969`, its `$5`): `member private M`,
        // `static member internal M`. Valid on a *concrete* member; an abstract
        // member rejects all accessibility (`parsAccessibilityModsIllegalForAbstract`,
        // FS561), so flag it there (FCS errors yet recovers, dropping the modifier).
        // Either way consume it as an `ACCESS_TOK` so it is captured (lossless); the
        // normaliser elides accessibility. A `new`-ctor's visibility is leading, not
        // name-position, so it is not handled here.
        if !is_new_ctor
            && matches!(
                self.peek(),
                Some((
                    Ok(FilteredToken::Raw(
                        Token::Internal | Token::Private | Token::Public
                    )),
                    _,
                ))
            )
        {
            if is_abstract {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "accessibility modifiers are not permitted on abstract members"
                        .to_string(),
                    span,
                });
            }
            self.bump_into(SyntaxKind::ACCESS_TOK);
        }
        // The member name. A `new`-ctor has no separate name token (the `new`
        // keyword above is the name). Gate on the *filtered* token so a pending
        // layout close is not seen through (cf. [`Self::parse_abstract_slot_at`]).
        if is_new_ctor {
            // no `IDENT` — the name is "new", derived by the normaliser.
        } else if self.at_active_pat_name() {
            // An active-pattern-named member sig — `member (|Foo|_|) : …`,
            // `static member (|Foo|Bar|) : …` (FCS's `opName` active-pattern
            // member name). Emit the `ACTIVE_PAT_NAME` node directly as the
            // `SynValSig` name (the same node the `val`-sig / pattern positions
            // use); the normaliser folds its case tokens to FCS's single `idText`
            // (`"|Foo|_|"`). Checked before the operator head: both open with `(`,
            // but only its second token is the `|` that `peek_operator_head`
            // excludes.
            self.parse_active_pat_name();
        } else if let Some(is_star) = self.peek_operator_head() {
            // An operator-named member sig — `member (+) : …`, `static member
            // (*) : …` (FCS's `opName` member name). Reuse the binding-head
            // operator machinery, which emits `[LPAREN_TOK, IDENT_TOK(op),
            // RPAREN_TOK]` directly as the `SynValSig` name (the source operator
            // spelling under `IDENT_TOK`; the differential normaliser unwraps
            // FCS's mangled `op_*` + `OriginalNotation` to match). No curried
            // args follow a *signature* name — the arg types live in the
            // `: <type>` below — so only the name is consumed (explicit typars, if
            // any, are taken by the shared postfix-typar parse after this chain).
            if is_star {
                self.consume_star_op_value();
            } else {
                self.consume_paren_op_value();
            }
        } else if matches!(
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
                message: "expected a member name".to_string(),
                span,
            });
        }
        // Optional explicit member type parameters after the name — FCS's
        // `classMemberSpfn` `opt_explicitValTyparDecls` (`member M<'U> : …`,
        // `member inline Bind< ^A, ^Aw> : …`), stored in
        // `SynValSig.explicitTypeParams` (the same `SynValTyparDecls` a `val f<'T>`
        // / `abstract M<'U>` sig carries). Reuse the shared postfix-typar parser
        // into the open `VAL_SIG`; an inside-`<>` `when` constraint folds into the
        // `TYPAR_DECLS` `PostfixList` exactly as the `val`-sig / type-header forms
        // do. The typars and their inside-`<>` constraints are elided by the
        // `AbstractSlot` projection (as for an `abstract M<'U>` slot), so consuming
        // them is all that is needed; the *after-type* `when` clause (plain or an
        // SRTP `(member …)` support constraint) is handled by the `topType` wrapper
        // below. The `<` is adjacent (the `HighPrecedenceTyApp` virtual) or spaced
        // (a bare `Less`); right after the member name a `<` can only open type
        // parameters. A `new`-ctor takes none (and its `new : …` has no `<`).
        //
        // `permit_empty = true`: like the `val`-sig sibling, FCS's
        // `explicitValTyparDeclsCore` admits an empty `member M< > : …` core.
        if !is_new_ctor
            && matches!(
                self.peek(),
                Some((
                    Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                        | FilteredToken::Raw(Token::Less(_))),
                    _,
                ))
            )
        {
            self.parse_typar_decls_postfix(true);
        }
        // The mandatory `: <type>` signature. FCS's `classMemberSpfn` member arm
        // ends in `COLON topTypeWithTypeConstraints` (`pars.fsy:969`), so a
        // trailing `when` clause folds the type into `SynType.WithGlobalConstraints`
        // and labelled parameters (`abstract M : x: int -> int`, phase 10.12b) are
        // admitted — route through `parse_type_with_constraints_top`, the `topType`
        // wrapper. In the SRTP `(member M : sig)` constraint context the `)` is
        // LexFilter-swallowed *after* the type, so no `when` follows and the
        // trailing-clause part is inert (named params still apply).
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
            self.bump_into(SyntaxKind::COLON_TOK);
            self.parse_type_with_constraints_top();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `:` and a type in a member signature".to_string(),
                span,
            });
        }
        // Optional `= <literal>` value (phase 10.12 member-literal) — FCS's
        // `classMemberSpfn` member arm ends in the same `optLiteralValueSpfn =
        // EQUALS declExpr` as the module-level `valSpfn`, storing the RHS in this
        // member's `SynValSig.synExpr`. Reuse `parse_let_equals_rhs` exactly as
        // `parse_val_sig_decl_at` does: consume the `=` + opening `OBLOCKBEGIN`,
        // gather the expression into the open `VAL_SIG`, and leave the closing
        // `OBLOCKEND` for the enclosing member-block loop so a following sibling
        // sig is reached. Gated on `!is_new_ctor`: FCS's `new`-ctor production
        // (`opt_access NEW COLON topType`) has no literal slot, so a `new : T = …`
        // stays an error rather than a lenient accept. Also gated on
        // `allow_literal` so the SRTP constraint context (whose swallowed `)`
        // exposes the enclosing binding's `=`) never claims it. (The `with
        // get/set` clause below is a distinct, mutually exclusive form, handled
        // after the close.)
        let had_literal_rhs = if allow_literal
            && !is_new_ctor
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Equals)), _))
            ) {
            self.parse_let_equals_rhs(false);
            true
        } else {
            false
        };
        self.builder.finish_node(); // VAL_SIG
        // An optional `with get[, set]` property-accessor clause (slice 3e) —
        // FCS records the `PropertyGet`/`Set`/`GetSet` kind in `flags` (elided by
        // the normaliser), so this is consumed only so it is not left to leak. The
        // `with` is the relabelled `Virtual::With` (`OWITH`); reuse the shared
        // [`Self::parse_member_sig_get_set_clause`] (also used by abstract slots).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::With)), _))
        ) {
            // A constructor sig (`NEW COLON topType`) has no accessor tail in
            // FCS's grammar — `new : T with get` is invalid. Flag it (so we, like
            // FCS, report an error) but still consume the clause so it is captured
            // inside the node (lossless) rather than leaking to the block loop.
            if is_new_ctor {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "a constructor signature cannot have `get`/`set` accessors"
                        .to_string(),
                    span,
                });
            }
            self.parse_member_sig_get_set_clause();
        }
        self.builder.finish_node(); // MEMBER_SIG
        had_literal_rhs
    }

    /// Parse a signature `inherit` member sig (`SynMemberSig.Inherit`) into the
    /// reused [`SyntaxKind::INHERIT_MEMBER`] node. Unlike the impl-side
    /// [`Self::parse_inherit_member`] (FCS's `inheritsDefn`: an `atomType` base
    /// then optional constructor args + `as` alias), the signature grammar is
    /// `INHERIT appTypeWithoutNull` — an *application* type (`inherit int list`,
    /// `inherit Base<int>`), no args, no alias. So this consumes the base via
    /// [`Self::parse_app_type`] and stops. The only children are the
    /// [`INHERIT_TOK`](SyntaxKind::INHERIT_TOK) and the base type; the normaliser
    /// reads the base from the type child (`base_type: Some(..)`). Caller has
    /// verified the cursor is at the raw `inherit` keyword.
    fn parse_sig_inherit_member(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INHERIT_MEMBER));
        self.bump_into(SyntaxKind::INHERIT_TOK);
        if self.peek_starts_atomic_type() {
            self.parse_app_type();
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
        self.builder.finish_node(); // INHERIT_MEMBER
    }
}
