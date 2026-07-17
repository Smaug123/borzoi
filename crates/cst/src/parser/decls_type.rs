//! Type-definition productions: the `type T …` header, optional implicit
//! constructor, type-parameter declarations and constraints, the type
//! representation dispatch ([`Parser::parse_type_defn_repr`]), and
//! `with`-augmentations. The structural reprs (record / union / enum /
//! exception) live in [`super::decls_repr`] and object-model member bodies in
//! [`super::decls_member`].

use super::*;

/// Which type production one `or`-separated SRTP support alternative is, per
/// FCS's grammar — see [`Parser::parse_type_alt_operand`], whose two callers
/// disagree because FCS's do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypeAltOperand {
    /// `appTypeWithoutNull` — the member *constraint*'s `typeAlts` operand
    /// (`when (^T or Witnesses) : (member …)`). A `| null` suffix is an FCS
    /// error here.
    WithoutNull,
    /// `appTypeCanBeNullable` — the trait-call *expression*'s `typarAlts`
    /// operand (`((^T or string | null) : (static member …) …)`), which is an
    /// `appTypeWithoutNull` plus an optional `| null`.
    CanBeNullable,
}

impl<'src> Parser<'src> {
    /// Phase 9.1/9.2 — parse a swallowed bare `type T = <typ>` definition,
    /// plus any `and`-chained continuations, into a [`SyntaxKind::TYPE_DEFNS`]
    /// carrier holding one or more [`SyntaxKind::TYPE_DEFN`]
    /// (`SynModuleDecl.Types [SynTypeDefn]`, `pars.fsy:2455`). The caller
    /// verified, via [`Self::raw_leading_type_defn`], that a swallowed `type`
    /// sits at the raw cursor.
    ///
    /// FCS aggregates an `and`-chain into **one** `Types` node and starts a
    /// fresh node at each new `type` keyword (`SyntaxTree.fsi:1768`), so the
    /// loop here collects the whole `type … and … and …` run; a following
    /// swallowed `type` is left for the enclosing `parse_module_decls` loop to
    /// start a separate `TYPE_DEFNS`. Unlike the swallowed `type`, `and` is a
    /// real filtered token — LexFilter keeps `CtxtTypeDefns` open across an
    /// aligned `and` (`isTypeContinuator`), so it reaches us directly and is
    /// claimed as `AND_TOK`.
    ///
    /// Only the type-abbreviation form is parsed; record / union / enum /
    /// object-model bodies are later phase-9 slices.
    pub(super) fn parse_type_defn(&mut self) {
        self.parse_type_defn_at(None);
    }

    /// Parse a `type … (and …)*` group. With `cp = None` this is the plain
    /// form; with `cp = Some(checkpoint)` the caller has already emitted one or
    /// more leading `ATTRIBUTE_LIST`s (phase 10.7a) after the checkpoint, and
    /// this wraps them — together with the group and its first definition — so
    /// the attributes become leading children of the **first** `TYPE_DEFN`
    /// (FCS attaches a type-header attribute to the first definition's
    /// `SynComponentInfo.attributes`; an `and`-chained definition carries its
    /// own, here always empty). Mirrors [`Self::parse_let_decl_at`].
    pub(super) fn parse_type_defn_at(&mut self, cp: Option<rowan::Checkpoint>) {
        let (kw, type_span) = self
            .next_non_trivia_raw_at_pos_with_span()
            .expect("caller verified a swallowed `type`");
        debug_assert!(
            matches!(kw, Token::Type),
            "parse_type_defn invoked without a swallowed raw `type`",
        );
        // First definition — leading swallowed `type`. Claim it directly from
        // the raw stream (it never reached the filtered stream, so `bump_into`
        // would mark it ERROR).
        match cp {
            // Attributed form: the `ATTRIBUTE_LIST`s sit between the checkpoint
            // and here. Open *both* the group and the first definition at `cp`
            // (later `start_node_at` at the same checkpoint nests outside), so
            // the attrs land as leading children of the first `TYPE_DEFN`. The
            // whitespace between the closing `>]` and `type` belongs inside the
            // definition (after the attrs), so drain it once the node is open.
            Some(cp) => {
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TYPE_DEFNS));
                self.builder
                    .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::TYPE_DEFN));
                self.drain_raw_up_to(type_span.start);
            }
            // Plain form: leading trivia stays a sibling of the decl node
            // (mirror `parse_nested_module_decl` / `parse_let_decl_at`).
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
        let (mut prev_closed, offside_attr) = self.parse_type_defn_name_and_body();
        self.builder.finish_node(); // TYPE_DEFN

        // Column-0 attributed regime: when a definition's header drains an
        // after-keyword attribute's trailing offside `BlockSep` (the
        // `type [<A>]⏎T` / `and [<B>]⏎U` form), the column-0 name makes that
        // definition's body blockless (no `OBLOCKEND`), so `prev_closed` never
        // trips. FCS still parses the chain here — the attribute's
        // `opt_OBLOCKSEP` licenses the column-0 layout, and `typeNameInfo`
        // (attribute included) applies to *every* `AND tyconDefn`, so a
        // continuation attribute can license its own blockless body just like
        // the first definition (ground-truthed via `fcs-dump`). Accumulate the
        // flag across the chain so a blockless body never severs it. The regime
        // is gated on an attribute — the bare `type⏎T = int⏎and U` (no
        // attribute) is an FCS error, and there `offside_attr` stays false so we
        // leave the `and` for the enclosing loop to flag, matching FCS.
        let mut column0_regime = offside_attr;

        // `and`-chained continuations. Each is its own `TYPE_DEFN` leading with
        // an `AND_TOK`. Inter-definition trivia (the newline before `and`)
        // stays a sibling *within* `TYPE_DEFNS`, so each `TYPE_DEFN` is tight
        // around its keyword/name/body — matching the first definition (whose
        // leading trivia was drained as a sibling above). A continuation is
        // taken when the previous body's offside block closed (`prev_closed`) —
        // a valid `and` is offside on its own line, so the prior block closed
        // before it — *or* when we are in the column-0 regime above (where the
        // bodies are blockless, so `prev_closed` is uninformative). An *inline*
        // `type T = int and U = …` with a block-bearing body keeps the `and`
        // inside the still-open first block, which FCS rejects, so with neither
        // condition met we leave it for the enclosing loop to flag rather than
        // splice a bogus chain.
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
            let (closed, offside_attr) = self.parse_type_defn_name_and_body();
            prev_closed = closed;
            column0_regime |= offside_attr;
            self.builder.finish_node(); // TYPE_DEFN
        }

        self.builder.finish_node(); // TYPE_DEFNS
    }

    /// Parse one type definition's `SynComponentInfo` header (name + optional
    /// type parameters) and its `= <repr>` body, after the leading keyword
    /// (`TYPE_TOK` / `AND_TOK`) has been claimed. Shared by the head and
    /// `and`-chained definitions in [`Self::parse_type_defn`]. Returns
    /// `(closed_block, offside_attr)`: whether the body's offside block closed
    /// (see [`Self::parse_type_defn_repr`]) and whether the header drained an
    /// after-keyword attribute's trailing offside `BlockSep` (the column-0
    /// offside-name form `type [<A>]⏎T`, which yields a blockless body — see
    /// [`Self::parse_type_defn_header`]).
    fn parse_type_defn_name_and_body(&mut self) -> (bool, bool) {
        let offside_attr = self.parse_type_defn_header();
        // Optional *after-decls* `when …` constraint clause
        // (`tyconNameAndTyparDecls opt_typeConstraints`, `pars.fsy:1605`) — FCS's
        // `SynComponentInfo.constraints`, as opposed to the inside-`<>` form
        // that lands in the `PostfixList`. Lands as a `TYPAR_CONSTRAINTS` child
        // of the open `TYPE_DEFN` node, before the repr's `=`.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::When)), _))) {
            self.parse_typar_constraints();
        }
        // The implicit primary constructor (phase 9.8a) follows the
        // `typeNameInfo` — *including* the after-decls `when` constraint:
        // FCS's `tyconDefn` is `typeNameInfo opt_simplePatterns …`, and
        // `typeNameInfo` ends with the `when` clause, so `type T<'a> when 'a :
        // equality (x: 'a) = …` is valid while `(x) when …` is a parse error
        // (ground-truthed against `fcs-dump`). Parsing the ctor here — after the
        // constraint, before the repr — matches that order.
        let ctor_span = self.parse_optional_implicit_ctor();
        // Augmentation `type T with member …` (phase 9.13a, FCS's
        // `tyconDefnAugmentation`): a raw `with` stands in for the `=`. The repr
        // is `ObjectModel(Augmentation, members=[])` and the members land in the
        // *outer* `SynTypeDefn.members` slot, so this is handled separately from
        // the `= <repr>` path. A primary constructor cannot precede `with` (FCS
        // rejects `type T(x) with …`), but we leave that diagnostic to a later
        // slice — the ctor still parses and the augment still recovers.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::With)), _))) {
            let closed_block = self.parse_type_defn_augmentation();
            return (closed_block, offside_attr);
        }
        // Bodyless type — `SynTypeDefnSimpleRepr.None`. With no `=` (and no
        // `with`), FCS reduces `tyconDefn` to its bare `typeNameInfo`
        // alternative (or, when a primary constructor precedes the absent `=`,
        // the `recover` alternative): both build a `Simple(None)` repr with **no
        // parse error** — `type Foo`, `[<Measure>] type m`, `type C(x)`. Emit no
        // repr node (the normaliser reads the missing repr as `None`) and no
        // diagnostic. The `recover` alternative routes a primary constructor to
        // the *outer* members slot with `implicitConstructor = None`, so a ctor
        // is accepted here too — skip the "only class types" diagnostic that the
        // `= <repr>` path below raises. Return `closed_block = true` so an
        // `and`-chained continuation on the next line is still taken: FCS keeps
        // `type m and n` in one `SynModuleDecl.Types` even when both bodies are
        // absent (ground-truthed via `fcs-dump`).
        if !matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Equals)), _))
        ) {
            return (true, offside_attr);
        }
        let (closed_block, is_object_model) = self.parse_type_defn_repr();
        // A primary constructor is only valid on a class (object-model) repr.
        // FCS still fills the `implicitConstructor` slot for `type R(x) = {…}` /
        // `type A(x) = int` but reports "Only class types may take value
        // arguments"; mirror that diagnostic (the tree shape already matches,
        // since we keep the ctor too). Anchored on the ctor's `(`.
        if let Some(span) = ctor_span
            && !is_object_model
        {
            self.errors.push(ParseError {
                message: "Only class types may take value arguments".to_string(),
                span,
            });
        }
        (closed_block, offside_attr)
    }

    /// Parse a type definition's `SynComponentInfo` name and optional type
    /// parameters (`SynTyparDecls`, phase 9.3):
    /// * **prefix** single typar `type 'a T` — the typar precedes the name
    ///   (`SynTyparDecls.SinglePrefix`);
    /// * **postfix** `type T<'a, 'b>` — `< … >` after the name
    ///   (`SynTyparDecls.PostfixList`), opened by the `HighPrecedenceTyApp`
    ///   virtual LexFilter emits before an adjacent `<`.
    ///
    /// Parenthesised-prefix `('a, 'b) T` (`SynTyparDecls.PrefixList`) is
    /// deferred: its `)` is LexFilter-swallowed, so it needs swallowed-closer
    /// recovery — a `(` here falls through to [`Self::parse_long_ident_path`],
    /// which records a clean "expected identifier" error (no panic).
    ///
    /// Returns whether an after-keyword attribute's trailing offside `BlockSep`
    /// (FCS's `opt_OBLOCKSEP`) was drained — i.e. this is the column-0 offside
    /// *name* form `type [<A>]⏎T`. The caller ([`Self::parse_type_defn_at`])
    /// uses that to keep the `and`-chain going across the resulting blockless
    /// bodies (see the loop there).
    pub(super) fn parse_type_defn_header(&mut self) -> bool {
        // Type-header attributes in `typeNameInfo` position (phase 10.7a) — the
        // attribute *after* the keyword on the **same line**: `type [<A>] T = …`
        // / `and [<B>] U = …`. FCS stores these in the same
        // `SynComponentInfo.attributes` as the before-`type` form
        // (`[<A>] type T`), so emit them as leading `ATTRIBUTE_LIST` children of
        // the already-open `TYPE_DEFN`. The before-`type` lists were consumed by
        // the dispatch before this runs, so the cursor here is at the name (no
        // double-parse); this fires only when the attribute follows the
        // `type`/`and` keyword. Runs for the first and each `and`-chained
        // definition.
        //
        // The *offside-name* form `type [<A>]⏎T` (the name on a fresh line,
        // aligned at column 0, after an after-keyword attribute) is FCS-valid:
        // the attribute production's trailing `opt_OBLOCKSEP` absorbs the
        // inter-line `BlockSep` that the column-0 name emits. A column-0 name
        // makes the `= …` body *blockless* (LexFilter emits no `OBLOCKBEGIN` /
        // `OBLOCKEND`) — contrary to an earlier belief that this `BlockSep` was
        // the body block's opener; it is not — so draining it (zero-width
        // `ERROR`) lets the name parse cleanly, mirroring FCS. The `and`-chain
        // continuation after such a blockless body is handled separately (the
        // loop in `parse_type_defn_at`), which is why we report the drain back.
        let mut drained_offside_attr_sep = false;
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) {
            self.parse_attribute_lists();
            while matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            ) {
                self.bump_into(SyntaxKind::ERROR);
                drained_offside_attr_sep = true;
            }
        }
        // Type-header accessibility (`type internal Foo`, `and private U`):
        // FCS's `tyconNameAndTyparDecls: opt_access path` (`pars.fsy:2543`)
        // accepts a `private`/`internal`/`public` modifier *before* the name
        // (and before a prefix typar). It lands in `SynComponentInfo.accessibility`
        // (field 6), which the normaliser elides on both sides, so consume it as
        // an `ACCESS_TOK` child of the open `TYPE_DEFN` (a sibling token of the
        // name's `LONG_IDENT`, the same convention as every other accessibility
        // site). This is the *type's* own access — distinct from the after-name
        // `opt_access` slot ([`Self::parse_optional_implicit_ctor`]), which is
        // either the primary constructor's accessibility (`type C private (x)`)
        // or, with no ctor args (`type C private = …`), discarded by FCS. The
        // grammar takes `opt_access` unconditionally here, and the keywords are
        // reserved, so no follow-token gating is needed.
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Internal | Token::Private | Token::Public
                )),
                _,
            ))
        ) {
            self.bump_into(SyntaxKind::ACCESS_TOK);
        }
        // Prefix single typar: a leading `'`/`^` sigil before the name.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Quote | Token::Op("^"))), _))
        ) {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPAR_DECLS));
            self.parse_typar_decl(true);
            self.builder.finish_node();
            self.parse_long_ident_path("type");
            return drained_offside_attr_sep;
        }

        // Name, then optional postfix `< … >` type parameters. The `<` may be
        // adjacent (`T<'a>`, preceded by the `HighPrecedenceTyApp` virtual) or
        // *spaced* (`T <'a>`, a bare raw `Less` with no virtual — FCS accepts it
        // with a "non-adjacent type parameters" warning, `pars.fsy:2578`'s
        // `opt_HIGH_PRECEDENCE_TYAPP`). Detect either; right after a type name a
        // `<` can only open type parameters.
        self.parse_long_ident_path("type");
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                    | FilteredToken::Raw(Token::Less(_))),
                _,
            ))
        ) {
            // A type definition uses FCS's `postfixTyparDecls`: an empty `T<>` is
            // a parse error, so `permit_empty = false`.
            self.parse_typar_decls_postfix(false);
        }
        drained_offside_attr_sep
    }

    /// Parse an optional implicit primary constructor `(args) [as self]` after a
    /// type definition's name/typars (phase 9.8a, FCS's `tyconDefn` alt 3
    /// `opt_simplePatterns`) into a [`SyntaxKind::IMPLICIT_CTOR`] node, a child
    /// of the open `TYPE_DEFN`.
    ///
    /// In FCS 43.x the constructor args are a regular `SynPat` (the
    /// `SynSimplePats` model was unified into `SynPat`), so we reuse the pattern
    /// parser: an empty `()` is a bare `SynPat.Const(SynConst.Unit)` (a
    /// `CONST_PAT` owning the `(`/`)`, *not* wrapped in a `PAREN_PAT` — verified
    /// against `fcs-dump`), while a non-empty `(…)` is a [`Self::parse_paren_pat`]
    /// `PAREN_PAT` (which already covers tuples and typed elements, e.g.
    /// `(x: int, y)`). An adjacent `(` (the common form) is preceded by the
    /// `HighPrecedenceParenApp` virtual, consumed zero-width here; a spaced
    /// `type T ()` has none.
    ///
    /// An optional accessibility modifier sits in FCS's `opt_access` slot before
    /// `opt_simplePatterns`. When constructor args `(…)` follow it is the
    /// constructor's accessibility (`type C private (x)`), consumed as an
    /// `ACCESS_TOK` inside the `IMPLICIT_CTOR`. When **no** args follow
    /// (`type C private = …`) FCS parses the modifier and then *discards* it —
    /// there is no `ImplicitCtor` and `SynComponentInfo.accessibility` stays
    /// `None`, so the AST equals `type C = …`; we mirror that by consuming it as
    /// a bare `ACCESS_TOK` sibling of the `TYPE_DEFN` and returning `None`. Both
    /// are elided by the normaliser. (The type's *own* before-name access —
    /// `type internal C` — is a different slot, handled in
    /// [`Self::parse_type_defn_header`], and *does* survive in the AST.)
    ///
    /// Returns the constructor's `(` span when a constructor was parsed (so the
    /// caller can flag a constructor on a non-class repr), else `None`.
    fn parse_optional_implicit_ctor(&mut self) -> Option<Range<usize>> {
        // Leading primary-constructor attributes (phase 10.7j): `type T [<A>] (…)`.
        // FCS's `typeNameInfo opt_attributes opt_access opt_simplePatterns` homes
        // them in `SynMemberDefn.ImplicitCtor.attributes` (field 1). They are only
        // a *ctor* carrier when a `(` (optionally after an `opt_access` modifier)
        // follows: with no ctor args FCS parses the attributes and then discards
        // them (no `ImplicitCtor`, empty `ComponentInfo`). So scan past the
        // `[<…>]` run on the raw stream first and only engage when a `(` follows —
        // otherwise leave the `[<` for the repr (matching the pre-existing
        // no-ctor handling). The run is opened under a checkpoint so the lists
        // become leading children of the `IMPLICIT_CTOR` opened below.
        let attr_cp = if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) && {
            let mut sig = self.significant_raw_from_cursor().peekable();
            while matches!(sig.peek(), Some(Token::LBrackLess)) {
                sig.next(); // `[<`
                for t in sig.by_ref() {
                    if matches!(t, Token::GreaterRBrack) {
                        break;
                    }
                }
            }
            match sig.next() {
                Some(Token::LParen) => true,
                Some(Token::Internal | Token::Private | Token::Public) => {
                    matches!(sig.next(), Some(Token::LParen))
                }
                _ => false,
            }
        } {
            let cp = self.builder.checkpoint();
            self.parse_attribute_lists();
            // The offside `type T [<A>]⏎  (…)` layout leaves an `OBLOCKSEP` between
            // the attribute and the `(`; skip it so the ctor detection below sees
            // the paren (FCS still homes the attribute on the `ImplicitCtor` here).
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

        // An accessibility modifier counts as the constructor's only when a `(`
        // follows it on the raw stream (else it is *type* accessibility, a
        // separate pre-existing gap — `type C private = …` has no constructor).
        let has_access = if let Some((
            Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
            span,
        )) = self.peek()
        {
            let end = span.end;
            matches!(self.next_non_trivia_raw_after(end), Some(Token::LParen))
        } else {
            false
        };

        // The constructor opens on a `(` — adjacent (an HPA virtual then
        // `LParen`), spaced (a bare `LParen`), or (after an access modifier) the
        // `LParen` past it. Anything else: no constructor.
        let at_hpa = matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceParenApp)),
                _
            ))
        );
        let at_lparen = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LParen)), _))
        );
        if !has_access && !at_hpa && !at_lparen {
            // A standalone after-name accessibility modifier with no constructor
            // args (`type C private = …`): FCS's `opt_access` before an empty
            // `opt_simplePatterns`. FCS discards it (no `ImplicitCtor`;
            // `ComponentInfo.accessibility` stays `None`, so the AST equals
            // `type C = …`). Consume it as a bare `ACCESS_TOK` sibling of the
            // `TYPE_DEFN` (elided by the normaliser) so the repr parse reaches
            // the `=`; no `IMPLICIT_CTOR` node is opened.
            //
            // Gate on the modifier being immediately followed (modulo trivia) by
            // the repr's `=`: this `opt_access` lives in FCS's `… EQUALS …`
            // production, whereas the augmentation production
            // (`typeNameInfo WITH …`) has *no* `opt_access` slot. So
            // `type T private with …` is an FCS error ("Unexpected keyword
            // 'with' … Expected '='"); consuming `private` unconditionally would
            // expose the `with` and mis-parse a *valid* augmentation. Leaving it
            // unconsumed makes the repr parse error on the modifier instead,
            // mirroring FCS.
            if let Some((
                Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
                span,
            )) = self.peek().cloned()
                && matches!(
                    self.next_non_trivia_raw_after(span.end),
                    Some(Token::Equals)
                )
            {
                self.bump_into(SyntaxKind::ACCESS_TOK);
            }
            return None;
        }

        // Open at the attribute checkpoint (phase 10.7j) so any leading
        // `ATTRIBUTE_LIST`s become the `IMPLICIT_CTOR`'s leading children; else a
        // plain (unattributed) ctor node.
        match attr_cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::IMPLICIT_CTOR)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::IMPLICIT_CTOR)),
        }
        if has_access {
            self.bump_into(SyntaxKind::ACCESS_TOK);
        }
        // Skip the `HighPrecedenceParenApp` virtual of an adjacent `(` (an access
        // modifier or a space before the `(` means there is no HPA).
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceParenApp)),
                _
            ))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
        // The cursor now sits on the `(`.
        let lparen_span = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::LParen)), span)) => span.clone(),
            _ => {
                // Malformed (an access modifier with no following `(`, despite
                // the gate) — close the node and report no constructor.
                self.builder.finish_node();
                return None;
            }
        };
        // Empty `()` → a bare `Const(Unit)` whose `CONST_PAT` owns the `(`/`)`
        // (the swallowed `)` is recovered from the raw stream), mirroring the
        // expression-side unit literal. A non-empty paren is the ordinary
        // `PAREN_PAT`.
        if matches!(
            self.next_non_trivia_raw_after(lparen_span.end),
            Some(Token::RParen) | None
        ) {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::CONST_PAT));
            self.bump_into(SyntaxKind::LPAREN_TOK);
            self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
            self.builder.finish_node(); // CONST_PAT
        } else {
            self.parse_paren_pat();
        }

        // Optional `as <self-id>` (`optAsSpec`). `as` is a real `Token::As`.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::As)), _))) {
            self.bump_into(SyntaxKind::AS_TOK);
            if self
                .next_non_trivia_raw_at_pos()
                .is_some_and(|t| matches!(t, Token::Ident(_) | Token::QuotedIdent(_)))
            {
                self.bump_into(SyntaxKind::IDENT_TOK);
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected a self-identifier after `as`".to_string(),
                    span,
                });
            }
        }
        self.builder.finish_node(); // IMPLICIT_CTOR
        Some(lparen_span)
    }

    /// Parse postfix `< 'a, 'b >` type-parameter declarations into a
    /// [`SyntaxKind::TYPAR_DECLS`] node. The caller has verified the cursor is
    /// at the `HighPrecedenceTyApp` virtual (adjacent `<`) or a bare raw `Less`
    /// (spaced `<`). Comma-separated [`Token::Quote`] / `^` typar decls between
    /// `<` and `>`.
    ///
    /// `permit_empty` selects between FCS's two grammars for the bracketed list:
    /// the type-definition `postfixTyparDecls` requires a **non-empty** list (an
    /// empty `T<>` is a parse error), while the value-binding
    /// `explicitValTyparDeclsCore` *permits* an empty `< >` (`let f< > x = x` is
    /// valid, producing `Some(SynValTyparDecls(Some(PostfixList [])))`). When
    /// `permit_empty` is set, an empty list closed immediately by `>` is accepted
    /// silently; a non-`>` token after `<` is still junk and errors in either
    /// mode. (The `, ..` flex list — the other `explicitValTyparDeclsCore`
    /// extension — is still unmodelled; it stops cleanly at the close check.)
    pub(super) fn parse_typar_decls_postfix(&mut self, permit_empty: bool) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPAR_DECLS));
        // Consume the `HighPrecedenceTyApp` virtual *if present* (it is emitted
        // only before an adjacent `<`; the spaced `T <'a>` form has none — the
        // HPA is `opt_` in FCS's `postfixTyparDecls`). Shares the `<`'s span, so
        // it lands as a zero-width ERROR, mirroring `parse_app_type`.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
        let opened_less = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Less(_))), _))
        );
        if opened_less {
            self.bump_into(SyntaxKind::LESS_TOK);
        }
        // The `typarDeclList`. FCS's `postfixTyparDecls` requires it **non-empty**
        // (an empty `T<>` or a missing close `T<'a =` is a parse error, not a
        // zero-param generic); the value-binding `explicitValTyparDeclsCore`
        // permits an empty `< >`. So record the "expected a type parameter" error
        // only when the list is absent *and* it isn't a `permit_empty` empty list
        // closed by `>` — a non-`>` token after `<` is junk in either mode (full
        // recovery is phase 11).
        // A typar decl starts at the `'`/`^` sigil or, in the attributed form
        // (`[<Measure>] 'a`), at a leading attribute `[<` — FCS's `typarDecl:
        // attributes typar`. The attribute run is parsed inside `parse_typar_decl`.
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Quote | Token::Op("^") | Token::LBrackLess
                )),
                _
            ))
        ) {
            self.parse_typar_decl(true);
            while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Comma)), _))) {
                self.bump_into(SyntaxKind::COMMA_TOK);
                // A following typar may be offside on the next line
                // (`type T<'a,⏎    'b>`); LexFilter inserts a `BlockSep` after
                // the comma. Skip it (zero-width ERROR) so the next typar is
                // seen — FCS accepts the multiline list. (No `BlockSep` precedes
                // the *first* typar, even offside, so only the comma needs this.)
                while matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
                ) {
                    self.bump_into(SyntaxKind::ERROR);
                }
                self.parse_typar_decl(true);
            }
        } else if opened_less {
            let next_is_close = matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Greater(_))), _))
            );
            if !(permit_empty && next_is_close) {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected a type parameter (`'a`) after `<`".to_string(),
                    span,
                });
            }
        }
        // Optional `when …` constraint clause inside the brackets
        // (`postfixTyparDecls: … typarDeclList opt_typeConstraints GREATER`,
        // `pars.fsy:2578`) — FCS's `SynTyparDecls.PostfixList` constraints.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::When)), _))) {
            self.parse_typar_constraints();
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Greater(_))), _))
        ) {
            self.bump_into(SyntaxKind::GREATER_TOK);
        } else if opened_less {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `>` to close the type parameter list".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // TYPAR_DECLS
    }

    /// Parse one `SynTyparDecl` into a [`SyntaxKind::TYPAR_DECL`] node —
    /// `[ATTRIBUTE_LIST*, (QUOTE_TOK | HAT_TOK), IDENT_TOK, (AMP_TOK <type>)*]`:
    /// an optional leading attribute run (FCS's `SynTyparDecl.attributes`,
    /// `[<Measure>] 'a`), then the `SynTypar(ident, staticReq, _)` (`'a` →
    /// `None`, `^a` → `HeadType`), then — in a `<…>` declaration list — an
    /// optional `& <flexible-type>` run (FCS's `SynTyparDecl.intersectionConstraints`,
    /// `pars.fsy:2570`, `'t & #seq<int>`). Mirrors [`Self::parse_var_type`]'s
    /// typar handling (a type *variable* `SynType.Var`), but emits a typar
    /// *declaration* node.
    ///
    /// `allow_intersection` gates the trailing `& …` run: FCS's `typarDecl`
    /// admits it, but a bare `typar` (a constraint subject `'a : …`, a
    /// paren-alternative `(^a or ^b)`, a static-optimization condition) does not,
    /// so those callers pass `false` — an `&` there belongs to (or is a clean
    /// error in) the enclosing construct, never this declaration.
    pub(super) fn parse_typar_decl(&mut self, allow_intersection: bool) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPAR_DECL));
        // Leading attribute run — FCS's `SynTyparDecl.attributes` (`[<Measure>]
        // 'a`). Parse one `[< … >]` list at a time, re-gating each on the
        // raw-aligned `peek_at_type_attribute` (as `parse_signature_parameter`
        // does), and drain any inter-list / pre-sigil offside `BlockSep` as
        // zero-width ERRORs so the sigil match below lands on a real token.
        while self.peek_at_type_attribute() {
            self.parse_attribute_list();
            while matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            ) {
                self.bump_into(SyntaxKind::ERROR);
            }
        }
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Raw(Token::Quote)), _)) => {
                self.bump_into(SyntaxKind::QUOTE_TOK)
            }
            Some((Ok(FilteredToken::Raw(Token::Op("^"))), _)) => {
                self.bump_into(SyntaxKind::HAT_TOK)
            }
            other => {
                let span = other
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected type-variable declaration (`'a` or `^a`)".to_string(),
                    span,
                });
                self.builder.finish_node();
                return;
            }
        }
        // The typar name. Gate on the next non-trivia *raw* token (mirror
        // `parse_var_type`): a sigil with no following ident is a clean error,
        // never a corrupting reach past a swallowed token.
        if self
            .next_non_trivia_raw_at_pos()
            .is_some_and(|t| matches!(t, Token::Ident(_) | Token::QuotedIdent(_)))
        {
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
        // The `& <flexible-type>` intersection-constraint run (FCS's
        // `SynTyparDecl.intersectionConstraints`, `'t & #seq<int>`). Only in a
        // declaration list — a bare-typar caller passes `allow_intersection =
        // false`, and the run is a no-op anyway when no `&` follows.
        if allow_intersection {
            self.parse_intersection_constraint_run();
        }
        self.builder.finish_node(); // TYPAR_DECL
    }

    /// Parse the `when …` type-parameter constraint clause
    /// (`opt_typeConstraints`, `pars.fsy:2615`, phase 9.3b) into a
    /// [`SyntaxKind::TYPAR_CONSTRAINTS`] node: `WHEN_TOK` then an `and`-separated
    /// list of [`Self::parse_typar_constraint`]s. The caller has verified the
    /// cursor is at a `when`. Inter-token spaces land as siblings via the
    /// dual-stream bump, so no explicit drain is needed.
    pub(super) fn parse_typar_constraints(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPAR_CONSTRAINTS));
        self.bump_into(SyntaxKind::WHEN_TOK);
        self.parse_typar_constraint();
        while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::And)), _))) {
            self.bump_into(SyntaxKind::AND_TOK);
            self.parse_typar_constraint();
        }
        self.builder.finish_node(); // TYPAR_CONSTRAINTS
    }

    /// FCS's `typeWithTypeConstraints` (`pars.fsy:6023`): a type, optionally
    /// followed by a trailing `when` constraint clause. When the `when` is
    /// present the type is wrapped in a [`SyntaxKind::CONSTRAINED_TYPE`] (FCS's
    /// `SynType.WithGlobalConstraints`); otherwise it is the bare type. The
    /// `when` group reuses [`Self::parse_typar_constraints`] verbatim — the same
    /// shape a type-definition header carries.
    ///
    /// Used only at grammar sites that admit *global* constraints (binding
    /// return info, typed patterns inside parens, signatures); the trailing-`when`
    /// check must stay out of the general [`Self::parse_type`] path, where a
    /// `when` belongs to an enclosing construct (a `match` guard, etc.). The
    /// trailing check is raw-stream-gated for exactly that reason — see the
    /// comment on the `when` branch below.
    pub(super) fn parse_type_with_constraints(&mut self) {
        self.parse_type_with_constraints_impl(false);
    }

    /// As [`Self::parse_type_with_constraints`], but in FCS's `topType` context
    /// (`topTypeWithTypeConstraints`, `pars.fsy:6030`) — a value / member /
    /// delegate signature type, where labelled parameters
    /// (`SynType.SignatureParameter`, phase 10.12b) are admitted. The base type is
    /// parsed via [`Self::parse_top_type`]; the optional trailing `when` clause is
    /// handled identically to the non-top form.
    pub(super) fn parse_type_with_constraints_top(&mut self) {
        self.parse_type_with_constraints_impl(true);
    }

    fn parse_type_with_constraints_impl(&mut self, top: bool) {
        // Mirror `parse_type`'s raw-stream guard *before* draining: a
        // LexFilter-swallowed `)` sits between `raw_pos` and the next filtered
        // token, so an eager drain would consume it as ERROR and a following
        // type-starter past the delimiter could be stolen as the type (the
        // recovery bug documented on `parse_type`). When the next non-trivia raw
        // is not a type-start, defer to `parse_type`, which emits the "expected
        // type" error and bails without crossing the delimiter; there is no base
        // type to attach a `when` clause to anyway. (Reachable binding-return
        // forms currently park a layout virtual at the swallowed `)`, so this is
        // defensive today, but the same helper will serve Stage-3 sites — typed
        // patterns / expressions — where no such virtual intervenes.)
        // In `top` context an optional parameter (`?x: int`) leads with `?`, and an
        // *attributed* parameter (`[<A>] x: int`) with `[<`; neither is a general
        // type-starter, so admit both via the sig-param lookahead so the
        // labelled-arg head reaches the `topType` path below.
        if !(self.peek_starts_type()
            || (top && (self.peek_is_signature_parameter() || self.peek_at_type_attribute())))
        {
            self.parse_type();
            return;
        }
        // The raw guard passed, so the next non-trivia raw *is* the type
        // starter (no swallowed delimiter before it). Drain leading raw trivia
        // (e.g. the space after the `:`) *before* the checkpoint, so a
        // `CONSTRAINED_TYPE` wrap starts at the first type token — preserving the
        // invariant that `Type::syntax().text_range()` begins at the type, not
        // the preceding whitespace.
        if let Some((_, next_span)) = self.peek() {
            let start = next_span.start;
            self.drain_raw_up_to(start);
        }
        let cp = self.builder.checkpoint();
        if top {
            self.parse_top_type();
        } else {
            self.parse_type();
        }
        // Attach a trailing `when` clause only when *both* cursors sit on the
        // `when` — they can disagree in two opposite ways, and each check rules
        // out one hazard:
        //
        // * The **filtered** cursor must be on `Raw(When)`. `parse_typar_constraints`
        //   opens with an unconditional `bump_into(WHEN_TOK)`, which labels
        //   whatever filtered token is current — so if a layout virtual sat
        //   between the type and the `when`, firing here would stamp the virtual
        //   as `WHEN_TOK` and orphan the real `when`. (This is the original gate.)
        // * The **raw** cursor (`next_non_trivia_raw_at_pos`) must also be on
        //   `When`. At a typed-pattern site the enclosing construct's `)` is
        //   LexFilter-swallowed, so the filtered stream collapses `(y: int)` + a
        //   following `match`-guard `when` into "type then `when`"; the filtered
        //   check alone would steal the guard (`(y: int) when y > 0 -> …`). The
        //   raw stream still carries the swallowed `)` as `RParen` between the
        //   type and the guard, so requiring raw `When` keeps the guard with its
        //   clause. (Genuine constraint sites — binding return info, val/member
        //   sigs, a parenthesised base type — consume the type's own `)` first,
        //   so the raw cursor is a bare `When` there.)
        // (The `'a :> T` subtype-constraint shorthand is handled one layer down,
        // in `parse_app_type_can_be_nullable` — FCS's `appTypeWithoutNull: typar
        // COLON_GREATER typ` sits below the tuple / arrow / nullable layers, so it
        // composes with them and with the trailing `when` wrap below.)
        let filtered_when = matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::When)), _)));
        let raw_when = matches!(self.next_non_trivia_raw_at_pos(), Some(Token::When));
        if filtered_when && raw_when {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::CONSTRAINED_TYPE));
            self.parse_typar_constraints();
            self.builder.finish_node(); // CONSTRAINED_TYPE
        }
    }

    /// Parse one `SynTypeConstraint` (`typeConstraint`, `pars.fsy:2652`) into a
    /// [`SyntaxKind::TYPAR_CONSTRAINT`] node — the subject typar
    /// ([`Self::parse_typar_decl`]) then the constraint operator/keyword. The
    /// supported forms (see [`SyntaxKind::TYPAR_CONSTRAINT`]): `:> T`, `: struct`,
    /// `: not struct`, `: null`, `: not null`, the `: comparison`/`equality`/
    /// `unmanaged` ident constraints, the `enum<…>`/`delegate<…>` and SRTP
    /// `(member …)` forms, and the subject-less self-constraint `when IFoo<'T>`
    /// ([`SyntaxKind::SELF_CONSTRAINT`]). The `default 'a : t` library-only form
    /// and a stray `default`/non-typar subject produce a recoverable error here,
    /// never a panic.
    /// `true` iff the cursor opens a parenthesised `typeAlts` SRTP constraint
    /// subject — *any* `(` in constraint-subject position, as in `(^a or ^b) :
    /// (member …)` and the general-type `(Witnesses or ^T) : (member …)`. FCS's
    /// LR grammar commits a `(` here to the `LPAREN typeAlts rparen COLON LPAREN
    /// classMemberSpfn rparen` production (`pars.fsy:2679`), so *every* `(`-led
    /// head routes to that branch — a `(` whose contents are not a valid
    /// `typeAlts` (`(IFoo)` with no member, `()`, `(^T or )`) is then a clean
    /// error, matching FCS's FS0010, rather than a self-constraint. (The
    /// `appTypeWithoutNull` alternatives themselves — including the two-token
    /// `struct (…)` / `{| … |}` heads and sign-folded static constants — are
    /// recognised by `parse_app_type` inside the branch, not by this
    /// single-token gate.)
    fn at_paren_type_alts(&self) -> bool {
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LParen)), _))
        )
    }

    /// Parse one `or`-separated SRTP support operand, after gating on a
    /// type-or-anon-record starter. Returns `true` iff an operand was parsed.
    ///
    /// The two SRTP supports take *different* operands, and the difference is
    /// observable: the member *constraint*'s `typeAlts` (`pars.fsy:2705`) takes
    /// `appTypeWithoutNull`, while the trait-call *expression*'s `typarAlts`
    /// (`pars.fsy:5547`) takes `appTypeCanBeNullable` — so FCS accepts
    /// `((^T or string | null) : (static member …) …)` but rejects the same
    /// `| null` in `when (^T or string | null) : (static member …)`. Hence
    /// [`TypeAltOperand`] rather than one shared production.
    ///
    /// The starter gate is load-bearing: an incomplete `(^T or )` or `()` leaves
    /// the cursor at the swallowed `)` / a `:`, and parsing a type there would
    /// reach `parse_atomic_type`'s `unreachable!` non-starter arm and *panic*;
    /// declining records a clean recoverable error instead. It is
    /// `peek_starts_type_or_anon_recd` — the exact accepted set of
    /// `parse_app_type` (atomic types *and* anon-records, but not a leading-slash
    /// measure, which is a full-`typ`-only starter it cannot consume); the
    /// nullable form only *extends* that with a trailing `| null`, so the same
    /// gate covers both.
    pub(super) fn parse_type_alt_operand(&mut self, operand: TypeAltOperand) -> bool {
        if self.peek_starts_type_or_anon_recd() {
            match operand {
                TypeAltOperand::WithoutNull => self.parse_app_type_without_null(),
                TypeAltOperand::CanBeNullable => self.parse_app_type_can_be_nullable(),
            }
            true
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a type alternative after `(` / `or` in an SRTP support \
                          constraint"
                    .to_string(),
                span,
            });
            false
        }
    }

    fn parse_typar_constraint(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPAR_CONSTRAINT));
        // Parenthesised `typeAlts` subject — `(^a or ^b) : (member …)` /
        // `(Witnesses or ^T) : (member …)` (FCS's `LPAREN typeAlts rparen COLON
        // LPAREN classMemberSpfn rparen`, `pars.fsy:2679`). Only valid before an
        // SRTP `: (member …)` constraint; the `)` is LexFilter-swallowed (like a
        // paren expression). Gated on a `(` opening an atomic-type head so a
        // malformed `(` still reaches `parse_typar_decl`'s clean error.
        //
        // FCS's `typeAlts` operands are `appTypeWithoutNull` — a typar `^a` (a
        // `SynType.Var`) *or* a concrete type (`Witnesses`, a `SynType.LongIdent`;
        // `IParsable<int>`, a `SynType.App`), so each alternative is a full
        // `parse_app_type` (via `parse_type_alt_operand`'s panic-guard), not a
        // typar declaration. The support is `Paren(Or(…))` on the FCS side; here
        // the operand `Type` nodes are direct children read via
        // `TyparConstraint::support_types`.
        if self.at_paren_type_alts() {
            self.bump_into(SyntaxKind::LPAREN_TOK);
            self.parse_type_alt_operand(TypeAltOperand::WithoutNull);
            while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Or)), _))) {
                self.bump_into(SyntaxKind::OR_TOK);
                if !self.parse_type_alt_operand(TypeAltOperand::WithoutNull) {
                    break;
                }
            }
            self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
            // FCS admits *only* a `: (classMemberSpfn)` member constraint after a
            // `typeAlts` subject — never the ordinary `struct` / `null` / `enum<…>`
            // forms. Flag a non-member RHS as an error (FCS rejects it), then still
            // consume it via the shared dispatch so the tree stays lossless.
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
                self.bump_into(SyntaxKind::COLON_TOK);
                let is_member = matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Raw(Token::LParen)), _))
                ) && self.paren_opens_member_sig_constraint();
                if !is_member {
                    let span = self
                        .peek()
                        .map(|(_, s)| s.clone())
                        .unwrap_or_else(|| self.source.len()..self.source.len());
                    self.errors.push(ParseError {
                        message:
                            "a parenthesised `(… or …)` constraint requires a `(member …)` signature"
                                .to_string(),
                        span,
                    });
                }
                self.parse_typar_constraint_after_colon();
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `:` and a member constraint after `(… or …)`".to_string(),
                    span,
                });
            }
            self.builder.finish_node(); // TYPAR_CONSTRAINT
            return;
        }
        // Bare self-constraint `when IFoo<'T>` / `when 'T` (F# 7 IWSAM
        // shorthand, FCS's `appTypeWithoutNull → WhereSelfConstrained(ty,
        // range)`, `pars.fsy:2702`). There is *no* subject typar — the
        // constraint head is an ordinary type. FCS's LR grammar reduces a
        // `typar COLON …` / `typar COLON_GREATER typ` head to the specific
        // subject-typar constraints and *every other* `appTypeWithoutNull` (an
        // ident-headed app, or a **bare** typar such as `'T` / `^T list`) to a
        // self-constraint. So a typar head is a subject only when immediately
        // followed by `:` / `:>`; any other non-`(` type head — bare typar
        // included — is the self-constraint. In constraint position a typar
        // carries no attributes, so it is exactly sigil+ident: the token two
        // significant positions on is the disambiguator. (The paren-typar-alts
        // branch above already claimed a `(`-then-sigil head; the general `(`
        // head is excluded below.)
        //
        // Parse with `parse_app_type` (the `appTypeWithoutNull` layer), *not*
        // `parse_type`: FCS's production sits below the tuple / arrow / nullable
        // layers, so `when IFoo<'T> * int` and `when IFoo<'T> -> int` are FCS
        // parse errors — full `parse_type` would wrongly accept them. Wrap the
        // result in a `SELF_CONSTRAINT` node so it is never read as the subtype
        // form's `:> T` constraint type.
        //
        // Gate on `peek_starts_type_or_anon_recd` — the predicate matching
        // `parse_app_type`'s atomic entry (`parse_atom_type_or_anon_recd_type`)
        // — *not* the looser `peek_starts_type`. The two differ only by a
        // leading-slash measure (`/s`, a full-`typ`-only reciprocal-measure
        // starter that `parse_atomic_type` would hit its `unreachable!` arm on);
        // FCS rejects a `/s` self-constraint too, so declining it here lets it
        // fall through to `parse_typar_decl`'s clean error rather than panic.
        let head_is_typar_sigil = matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Quote | Token::Op("^"))), _))
        );
        let typar_is_subject = head_is_typar_sigil
            && matches!(
                self.nth_significant_raw_at_pos(2),
                Some(Token::Colon | Token::ColonGreater)
            );
        // A `(`-led head never reaches here — `at_paren_type_alts` above claimed
        // *every* `(` (FCS commits a `(` at constraint position to the
        // `(typeAlts) : (member …)` production, `pars.fsy:2679`), so `(IFoo)`,
        // `(int)`, `('T)` are its clean errors, not self-constraints. (FCS does
        // accept a paren self-constraint whose contents are not a `typeAlts`,
        // e.g. `(IFoo -> int)`, via the `atomType` paren fallback, but that
        // exotic form stays a deferred we-reject.) So the self-constraint gate is
        // just "not a subject typar, and a `parse_app_type`-consumable head"
        // (`struct (` / `{|` self-constraints, which FCS *does* accept, still
        // reach it — their heads are `Token::Struct` / `Token::LBraceBar`).
        if !typar_is_subject && self.peek_starts_type_or_anon_recd() {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::SELF_CONSTRAINT));
            self.parse_app_type();
            self.builder.finish_node(); // SELF_CONSTRAINT
            self.builder.finish_node(); // TYPAR_CONSTRAINT
            return;
        }
        // Subject typar (`'a` / `^a`) followed by `:` / `:>`. A non-typar /
        // non-type head (`default 'a : t`, a stray token) records a clean error
        // inside `parse_typar_decl`.
        self.parse_typar_decl(false);
        match self.peek().cloned() {
            // `'a :> T` — subtype constraint; the type reuses `parse_type`
            // (self-guarding, self-draining).
            Some((Ok(FilteredToken::Raw(Token::ColonGreater)), _)) => {
                self.bump_into(SyntaxKind::COLON_GREATER_TOK);
                self.parse_type();
            }
            Some((Ok(FilteredToken::Raw(Token::Colon)), _)) => {
                self.bump_into(SyntaxKind::COLON_TOK);
                self.parse_typar_constraint_after_colon();
            }
            other => {
                let span = other
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `:` or `:>` in a type-parameter constraint".to_string(),
                    span,
                });
            }
        }
        self.builder.finish_node(); // TYPAR_CONSTRAINT
    }

    /// After a `delegate` keyword or `enum` ident in a typar constraint, consume
    /// the mandatory `< … >` type-argument list (FCS's `typeArgsNoHpaDeprecated`,
    /// `pars.fsy:2684`/`2690`) into a [`SyntaxKind::CONSTRAINT_TYPE_ARGS`] wrapper
    /// child of the `TYPAR_CONSTRAINT` — kept separate from the subtype form's
    /// direct constraint type so `TyparConstraint::ty` / `type_args` never
    /// conflate the two. The list is required — a bare `'a : enum` /
    /// `'a : delegate` is an FCS parse error — so a missing `<` records a clean
    /// error (and no wrapper).
    fn consume_constraint_type_args(&mut self, name: &str, name_span: Range<usize>) {
        if self.at_type_args_no_hpa() {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::CONSTRAINT_TYPE_ARGS));
            self.consume_type_args_no_hpa();
            self.builder.finish_node(); // CONSTRAINT_TYPE_ARGS
        } else {
            let span = self.peek().map(|(_, s)| s.clone()).unwrap_or(name_span);
            self.errors.push(ParseError {
                message: format!("expected a `<…>` type-argument list after `{name}` constraint"),
                span,
            });
        }
    }

    /// Parse the right-hand side of a `'a : …` constraint, after the `:` has
    /// been consumed: `struct` / `null` keywords, an `not struct`/`not null`
    /// (the `not` is a plain `IDENT`), a `comparison`/`equality`/`unmanaged`
    /// ident, or the type-argument-list forms `enum<…>` (`WhereTyparIsEnum`) and
    /// `delegate<…>` (`WhereTyparIsDelegate`). Other idents (an unknown name) and
    /// other tokens are unsupported here (`pars.fsy:2660`–`2703`); they record a
    /// clean error matching FCS's `parsUnexpectedIdentifier` shape.
    fn parse_typar_constraint_after_colon(&mut self) {
        match self.peek().cloned() {
            // `^T : (static member M : sig)` — an SRTP member constraint (FCS's
            // `WhereTyparSupportsMember`, `pars.fsy:2695`). The parenthesised
            // member signature reuses the shared `parse_member_sig`; the `)` is
            // LexFilter-swallowed (like a paren expression), so it is claimed
            // from the raw stream via `bump_swallowed_closer`. The `MEMBER_SIG`
            // becomes a child of the `TYPAR_CONSTRAINT`, where
            // `TyparConstraint::member_sig` reads it. Gate on a member-sig start
            // after the `(` so an ordinary parenthesised constraint type (none
            // is valid here today) still reaches the error arm.
            Some((Ok(FilteredToken::Raw(Token::LParen)), _))
                if self.paren_opens_member_sig_constraint() =>
            {
                self.bump_into(SyntaxKind::LPAREN_TOK);
                self.parse_member_sig();
                self.bump_swallowed_closer(
                    SyntaxKind::RPAREN_TOK,
                    |t| matches!(t, Token::RParen),
                    ")",
                    "member constraint",
                );
            }
            Some((Ok(FilteredToken::Raw(Token::Struct)), _)) => {
                self.bump_into(SyntaxKind::STRUCT_TOK);
            }
            Some((Ok(FilteredToken::Raw(Token::Null)), _)) => {
                self.bump_into(SyntaxKind::NULL_TOK);
            }
            // `'a : delegate<args, ret>` — `WhereTyparIsDelegate` (`pars.fsy:2684`).
            // `delegate` is a keyword (`Token::Delegate`), not an ident, so it has
            // its own arm; the mandatory `< … >` list follows.
            Some((Ok(FilteredToken::Raw(Token::Delegate)), span)) => {
                self.bump_into(SyntaxKind::DELEGATE_TOK);
                self.consume_constraint_type_args("delegate", span);
            }
            // An identifier constraint — classified by its *de-quoted* text, so a
            // backticked `` ``comparison`` `` / `` ``not`` `` is the same constraint
            // as the bare form (FCS stores `Ident.idText` without backticks). A
            // backticked keyword (`` ``struct`` ``) is therefore a plain ident, not
            // the `struct` constraint — it lands in the unknown-ident arm, matching
            // FCS's `parsUnexpectedIdentifier`.
            Some((Ok(FilteredToken::Raw(tok)), span))
                if matches!(tok, Token::Ident(_) | Token::QuotedIdent(_)) =>
            {
                let bare = ident_token_text(&tok).expect("matched an identifier token");
                match bare {
                    // `not struct` / `not null` — FCS spells `not` as a bare
                    // `IDENT` before the `STRUCT`/`NULL` keyword (`pars.fsy:2663`/
                    // `2670`).
                    "not" => {
                        self.bump_into(SyntaxKind::IDENT_TOK);
                        match self.peek().cloned() {
                            Some((Ok(FilteredToken::Raw(Token::Struct)), _)) => {
                                self.bump_into(SyntaxKind::STRUCT_TOK);
                            }
                            Some((Ok(FilteredToken::Raw(Token::Null)), _)) => {
                                self.bump_into(SyntaxKind::NULL_TOK);
                            }
                            other => {
                                let span = other
                                    .map(|(_, s)| s)
                                    .unwrap_or_else(|| self.source.len()..self.source.len());
                                self.errors.push(ParseError {
                                    message:
                                        "expected `struct` or `null` after `not` in a constraint"
                                            .to_string(),
                                    span,
                                });
                            }
                        }
                    }
                    "comparison" | "equality" | "unmanaged" => {
                        self.bump_into(SyntaxKind::IDENT_TOK);
                    }
                    // `'a : enum<'b>` — `WhereTyparIsEnum` (`pars.fsy:2690`). `enum`
                    // is a bare ident (not a keyword) followed by the mandatory
                    // `< … >` type-argument list.
                    "enum" => {
                        self.bump_into(SyntaxKind::IDENT_TOK);
                        self.consume_constraint_type_args("enum", span);
                    }
                    // An unknown name is an FCS parse error — consume the ident for
                    // losslessness and record it.
                    _ => {
                        self.errors.push(ParseError {
                            message: format!("unsupported or unexpected type constraint `{bare}`"),
                            span,
                        });
                        self.bump_into(SyntaxKind::IDENT_TOK);
                    }
                }
            }
            other => {
                let span = other
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected a type constraint after `:`".to_string(),
                    span,
                });
            }
        }
    }

    /// Parse `= OBLOCKBEGIN <repr> OBLOCKEND` after a type definition's name —
    /// the `SynTypeDefnSimpleRepr`. Dispatches on the body's first token:
    /// * `{` → a record (`SynTypeDefnSimpleRepr.Record`, phase 9.4), via
    ///   [`Self::parse_record_repr`];
    /// * otherwise → a type abbreviation (`TypeAbbrev`, phase 9.1), FCS's full
    ///   `typ` (`tyconDefnOrSpfnSimpleRepr: opt_attributes opt_access typ`,
    ///   `pars.fsy:2455`) wrapped in a [`SyntaxKind::TYPE_ABBREV`] node.
    ///
    /// The opening `OBLOCKBEGIN` is consumed as a zero-width ERROR placeholder
    /// (mirror `parse_let_equals_rhs`); the closing `OBLOCKEND` is consumed
    /// advancing only `pos` (no raw drain), so a following swallowed
    /// `type`/`and` keyword that shares its byte span survives for the next decl
    /// (the `parse_nested_module_body` discipline).
    ///
    /// Members may trail the repr (phase 9.13b), routed to the **outer**
    /// `SynTypeDefn.members` slot: *bare* members (an `OBLOCKSEP` then a
    /// member-block item, before the body close — admitted per repr arm) and/or
    /// a `with`-augment block (after the body close — admitted everywhere); see
    /// the two hooks below.
    ///
    /// Returns `(closed_block, is_object_model)`. `closed_block` is `true` iff
    /// the body opened **and** closed its offside block — the `and`-continuation
    /// gate ([`Self::parse_type_defn`]): a valid chain has each body offside (its
    /// own block, closed by an `OBLOCKEND` before the `and`), whereas an
    /// *inline* `type T = int and U = …` keeps the `and` inside the first body's
    /// still-open block — FCS rejects that ("Unexpected keyword 'and' in member
    /// definition"), so we must not splice it into a bogus chain.
    /// `is_object_model` is `true` iff the body was an object model (`member …`),
    /// which the caller uses to flag a primary constructor on a non-class repr
    /// (FCS's "Only class types may take value arguments", phase 9.8a).
    fn parse_type_defn_repr(&mut self) -> (bool, bool) {
        // The repr's `=`. The caller ([`Self::parse_type_defn_name_and_body`])
        // only routes here once it has seen the `=`; a bodyless type (no `=`) is
        // handled there as `SynTypeDefnSimpleRepr.None`, so the token is
        // guaranteed present.
        debug_assert!(
            matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::Equals)), _))
            ),
            "parse_type_defn_repr requires a leading `=`",
        );
        self.bump_into(SyntaxKind::EQUALS_TOK);

        // Opening `OBLOCKBEGIN` — consume as a zero-width ERROR (mirror
        // `parse_let_equals_rhs`). Absent only on malformed input.
        let opened_block = if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
            true
        } else {
            false
        };

        let mut is_object_model = false;
        // Whether the repr was the explicit `class`/`struct`/`interface … end`
        // form (phase 9.12). Unlike the lightweight forms, it emits an extra
        // `OBLOCKSEP` inside the type-defn block before that block's closing
        // `OBLOCKEND` — see the `closed_block` handling below.
        let mut explicit_end_form = false;
        // Whether the parsed repr was a delegate body (`delegate of <type>`).
        // FCS forbids a trailing augmentation on a delegate
        // (`parsAugmentationsIllegalOnDelegateType`); the bare-members / `with`
        // hooks below record that, keyed off this flag.
        let mut is_delegate = false;
        // Whether the parsed repr admits *bare* trailing members (phase 9.13b,
        // the no-`with` `type R =⏎ { … }⏎ member …` form): a record or enum
        // does, a union only when it carries a `|`, an abbreviation (or a pure
        // object model, whose member loop has already consumed everything)
        // never — ground-truthed against FCS per arm.
        let mut admits_bare_members = false;
        if self.peek_is_record_repr_start() {
            // Record repr — `{ F : T1; … }`, optionally preceded by a repr-level
            // access modifier (`type T = private { … }`). The `{` is a real
            // token; the `}` is LexFilter-swallowed (recovered inside
            // `parse_record_repr`).
            self.parse_record_repr();
            admits_bare_members = true;
        } else if self.peek_is_union_or_enum_repr_start() {
            // Discriminated-union or enum repr — `[|] A | B of int`, or
            // `A = 0 | B = 1`. Must precede the abbreviation arm: a case name is
            // itself a type-starter, so `peek_starts_type_or_anon_recd` would
            // otherwise claim `A | B` as an abbreviation `A` plus a dangling
            // `| B`. The detection excludes `Ident | null` (a `WithNull`
            // abbreviation, 7.11); the Union-vs-Enum choice is post-hoc.
            admits_bare_members = self.parse_union_or_enum_repr();
        } else if self.peek_is_kind_marked_repr_start() {
            // Explicit kind marker — `class … end` / `struct … end` /
            // `interface … end` (phase 9.12). The `class`/`struct`/`interface`
            // keywords are raw tokens none of the other arms match (the
            // member-form `interface` is `Virtual::InterfaceMember`, handled by
            // the object-model arm, not raw `Token::Interface`). Sets the repr's
            // `SynTypeDefnKind` to `Class`/`Struct`/`Interface`; members reuse
            // 9.7–9.11 via `parse_member_block_items`, the body delimited by `end`.
            self.parse_kind_marked_repr();
            is_object_model = true;
            explicit_end_form = true;
        } else if self.peek_is_delegate_repr_start() {
            // Delegate body — `delegate of <topType>` (`pars.fsy:1779`). `delegate`
            // is a raw keyword token (`Token::Delegate`) that no other repr arm
            // matches, and it is not a `typ`-starter, so the abbreviation arm
            // below never claims it. FCS lowers this to an `ObjectModel(Delegate(
            // ty, arity), [Invoke], _)`; we keep the surface `DELEGATE_REPR`
            // shape. Marked object-model so a stray primary constructor
            // (`type D(x) = delegate of …`) is *folded in* — FCS prepends the
            // ctor to the delegate's members rather than flagging "Only class
            // types may take value arguments".
            self.parse_delegate_repr();
            is_object_model = true;
            is_delegate = true;
        } else if self.peek_is_inline_il_repr_start() {
            // FSharp.Core's inline-IL type body — `( # "instr" # )`
            // (`SynTypeDefnSimpleRepr.LibraryOnlyILAssembly`, `pars.fsy:2483`).
            // Must precede the abbreviation arm: `peek_starts_type` claims the
            // `(` as a paren-type start and then chokes on the `#`. The `(`/`)`
            // are owned by the `INLINE_IL_REPR` directly; the closing `)` is
            // LexFilter-swallowed and recovered. Leaves `admits_bare_members`
            // false (an inline-IL abbreviation admits no trailing members), like
            // the abbreviation arm below.
            self.parse_inline_il_repr();
        } else if self.peek_is_object_model_start() {
            // Object-model repr — `member …` (phase 9.7). `member` is a real
            // filtered keyword (`Token::Member`), distinct from every other body
            // shape, so this arm is unambiguous. The member-block loop consumes
            // each member's own RHS-close `OBLOCKEND` (and inter-member
            // `ODECLEND`/`OBLOCKSEP`); the body-closing `OBLOCKEND` is left for
            // the `closed_block` handling below.
            self.parse_object_model_repr();
            is_object_model = true;
        } else if self.peek_starts_type() {
            // Type abbreviation — FCS's full `typ`. Uses the `typ`-level
            // `peek_starts_type` (not the atomic gate) so a reciprocal-measure
            // abbreviation `type T = /s` (leading-`/` tuple, phase 10.9) reaches
            // `parse_type`. Object-model bodies are a later phase-9 slice, so any
            // other non-type body produces the clean "expected type" error below
            // (never a panic); `parse_type` self-guards and self-drains.
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::TYPE_ABBREV));
            self.parse_type();
            self.builder.finish_node();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected a type or `{ … }` record body after `=`".to_string(),
                span,
            });
        }

        // Bare trailing members (phase 9.13b) — FCS's #light
        // `tyconDefnRhs opt_OBLOCKSEP classDefnMembers` (`pars.fsy:1731`):
        // a member-block item *inside the still-open body block*, behind an
        // `OBLOCKSEP` (the offside `type R =⏎ { … }⏎ member …` layout — the
        // union/enum arms leave the separator at the cursor) or directly at the
        // cursor (the `opt_` in `opt_OBLOCKSEP`: the fully inline
        // `type R = { X: int } member …`, and the record layouts whose
        // separator was absorbed with the `}`-on-own-line close inside
        // `parse_record_repr`). The members are routed to the **outer**
        // `SynTypeDefn.members` slot (direct children of the open `TYPE_DEFN`,
        // like the 9.13a augmentation). The FCS-invalid single-line form
        // (`type R = { X: int }⏎ member …`) arrives with the body-close
        // `OBLOCKEND` *before* the member — the classifier's virtual guard
        // stops it — so LexFilter has already done the discrimination.
        //
        // A delegate (`admits_bare_members` stays false) admits no *valid* bare
        // members, but FCS still treats a trailing member block as an
        // (illegal) augmentation — `parsAugmentationsIllegalOnDelegateType`,
        // the same error as the explicit-`with` form below. So consume them
        // here too (clean recovery, one targeted error) rather than letting the
        // member spill to the module loop as generic "unexpected token" noise.
        let mut parsed_bare_members = false;
        if admits_bare_members || is_delegate {
            let at_sep_then_item = matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            ) && self.bare_trailing_member_follows();
            if at_sep_then_item {
                self.bump_into(SyntaxKind::ERROR); // the OBLOCKSEP
            }
            if at_sep_then_item || self.classify_object_model_item().is_some() {
                // The augmentation's start (the first member item), for the
                // delegate diagnostic below.
                let aug_span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.parse_member_block_items();
                parsed_bare_members = true;
                if is_delegate {
                    self.errors.push(ParseError {
                        message: DELEGATE_AUGMENTATION_ERROR.to_string(),
                        span: aug_span,
                    });
                }
            }
        }

        // An `open` indented *inside* the type body is invalid F# (FCS's
        // FS0058, "'open' declarations must appear at module level, not inside
        // types."). The lex-filter leaves such an `open` inside the still-open
        // body block — directly at the cursor (an object-model body, whose
        // member loop just broke on it) or behind the body's `OBLOCKSEP` (a
        // union/record/enum body, whose bare-member gate declined the
        // non-member) — i.e. *before* the body-closing `OBLOCKEND`. A dedented
        // module-level `open` instead arrives *after* that `OBLOCKEND` (the
        // body has already closed), so it never reaches here. Flag it; recovery
        // then leaves the stray `open` for the enclosing module loop — the same
        // stray-item discipline `bare_trailing_member_follows` documents — so
        // the green tree is unchanged and only the diagnostic is added. (Other
        // stray keywords the module loop already rejects on its own; `open` is
        // special precisely because it is *also* a valid module-level decl.)
        if opened_block && let Some(span) = self.stray_open_in_type_body_span() {
            self.errors.push(ParseError {
                message: "'open' declarations must appear at module level, not inside types."
                    .to_string(),
                span,
            });
        }

        // The explicit `class`/`struct`/`interface … end` form emits an extra
        // `OBLOCKSEP` (the indentation before `end`) inside the type-defn block,
        // appearing in the filtered stream *after* `end` and *before* the
        // block's closing `OBLOCKEND`. Its raw bytes were already drained as
        // `end`'s leading trivia, so it is a pure virtual marker here — skip it
        // (zero-width, advancing only `pos`, the `closed_block` discipline) so
        // the closing `OBLOCKEND` below is reached. Without this the type-defn
        // block's `OBLOCKEND` leaks to the enclosing module loop, which misreads
        // it as the *module body's* close and pops the following sibling decls
        // (`let …` / `and …`) out a nesting level. The lightweight/union/record
        // forms leave no such separator, so this is gated on the explicit-end
        // form. It is *also* gated on `opened_block` (like `closed_block` below):
        // in the column-0 offside-attribute regime (`type [<A>]⏎T = class … end`)
        // the body is blockless, so there is no type-defn block — and the
        // `OBLOCKSEP` after `end` is then the *module* declaration separator
        // before the sibling decl, which must be left for the module loop.
        if opened_block
            && explicit_end_form
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            )
        {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }

        // Closing `OBLOCKEND` — consume as a zero-width ERROR advancing only
        // `pos` (the `parse_nested_module_body` / `parse_if_body` discipline):
        // the raw cursor stays put so trailing trivia and a following swallowed
        // `type`/`and` keyword (which shares the `OBLOCKEND`'s byte span)
        // survive for the enclosing loop / next decl.
        let closed_block = opened_block
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
            );
        if closed_block {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }

        // Trailing `with`-augmentation (phase 9.13b) — FCS's `opt_classDefn`
        // (`pars.fsy:2320`), valid after *every* repr (abbreviation and pure
        // object model included) and whatever the `with`'s indentation
        // (LexFilter grants `with` undentation grace, so the body block has
        // closed and the raw `with` sits at the cursor in every layout). The
        // members go to the outer `SynTypeDefn.members` slot — direct children
        // of the open `TYPE_DEFN`, exactly like 9.13a — while the repr and its
        // kind stay unchanged, so the `WITH_TOK` is a plain direct child (not
        // 9.13a's `OBJECT_MODEL_REPR` `Augmentation` marker; the facade keys
        // the kind off that nesting). Both bare members *and* a `with` block is
        // FCS's `checkForMultipleAugmentations` error — record it and parse the
        // block anyway (lossless; FCS drops the whole declaration).
        if let Some((Ok(FilteredToken::Raw(Token::With)), with_span)) = self.peek().cloned() {
            if is_delegate && !parsed_bare_members {
                // FCS `parsAugmentationsIllegalOnDelegateType`: a delegate type
                // takes no augmentation. Record the error and parse the block
                // anyway (lossless; FCS drops the whole declaration). Skip when
                // a bare member block already flagged it above (a delegate with
                // both is doubly malformed; one diagnostic suffices).
                self.errors.push(ParseError {
                    message: DELEGATE_AUGMENTATION_ERROR.to_string(),
                    span: with_span.clone(),
                });
            }
            if parsed_bare_members {
                self.errors.push(ParseError {
                    message: "At most one 'with' augmentation is permitted".to_string(),
                    span: with_span,
                });
            }
            self.bump_into(SyntaxKind::WITH_TOK);
            // The augment owns the member block's close virtuals; its
            // closed-block result is what precedes a potential `and`
            // continuation, so it replaces the repr's.
            let aug_closed = self.parse_with_augmentation_members(false, true);
            return (aug_closed, is_object_model);
        }

        (closed_block, is_object_model)
    }

    /// `true` iff the `OBLOCKSEP` at the cursor is followed by a member-block
    /// item — the bare-trailing-members gate (phase 9.13b). A class-local `let`
    /// (offside [`Virtual::Let`]), a class-body `do` (offside [`Virtual::Do`],
    /// phase 9.8d), and a member-form `interface I [with …]` (offside
    /// [`Virtual::InterfaceMember`], the LexFilter relabel of the raw `interface`
    /// keyword) all arrive as a virtual right after the separator; every other
    /// item head is a raw keyword, classified by the raw scan (the separator is a
    /// layout virtual with no raw counterpart, so the scan already starts at the
    /// item's first token). These three relabels are virtual-only, so the raw
    /// scan can't see them — they must be matched on the filtered stream here.
    /// (A bare trailing `do` after a record/union/enum repr — `type R =`⏎`  {
    /// … }`⏎`  do …` — is FCS's `classDefnMembers`, routed to the outer
    /// `SynTypeDefn.members`; the FCS-invalid `=`-line form `type R = { … }`⏎`  do
    /// …` instead has the body-close `OBLOCKEND` *before* the `do`, so the
    /// `peek() is BlockSep` caller gate declines and the `do` errors, mirroring
    /// FCS.) When the gate declines, the separator is left for the enclosing
    /// module loop, whose next decl parse flags the stray item — matching FCS's
    /// "Unexpected keyword 'member'" class of errors (e.g. after an abbreviation,
    /// or a zero-bar union).
    fn bare_trailing_member_follows(&self) -> bool {
        matches!(
            self.next_non_trivia_filtered_after_pos(),
            Some(FilteredToken::Virtual(
                Virtual::Let | Virtual::InterfaceMember | Virtual::Do
            ))
        ) || self.classify_object_model_item_from_raw().is_some()
    }

    /// The byte span of an `open` keyword sitting *inside* a still-open type
    /// body block, if the cursor is at one. Used by [`Self::parse_type_defn_repr`]
    /// to flag FS0058 ("`open` … inside types"). The `open` is either directly at
    /// the cursor (an object-model member loop just broke on it) or one
    /// `OBLOCKSEP` ahead (a union/record/enum body left the separator when its
    /// bare-member gate declined the non-member); in both cases the body-closing
    /// `OBLOCKEND` is still ahead, which is exactly what tells a body-internal
    /// `open` apart from a dedented module-level one (whose `OBLOCKEND` would
    /// already have been consumed). Returns `None` for anything else (including
    /// the legitimate body-close `OBLOCKEND`).
    fn stray_open_in_type_body_span(&self) -> Option<Range<usize>> {
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Open)), span)) => Some(span.clone()),
            // Behind the body's `OBLOCKSEP`: scan past the separator (and any
            // trivia) to the first significant filtered token and accept it only
            // if it is the `open`.
            Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _)) => self
                .filtered_tokens
                .iter()
                .skip(self.pos + 1)
                .find_map(|(res, span)| match res {
                    Ok(FilteredToken::Raw(t)) if trivia_kind(t).is_some() => None,
                    Ok(FilteredToken::Raw(Token::Open)) => Some(Some(span.clone())),
                    _ => Some(None),
                })
                .flatten(),
            _ => None,
        }
    }

    /// Parse a type augmentation body `with member …` (phase 9.13a) — FCS's
    /// `tyconDefnAugmentation`, repr `ObjectModel(Augmentation, members=[])`.
    /// The `with` (a raw token) replaces the `=`; the members live in the
    /// **outer** `SynTypeDefn.members` slot, so they are emitted as direct
    /// children of the open `TYPE_DEFN` (not wrapped in the repr). The repr
    /// itself is an *empty* [`SyntaxKind::OBJECT_MODEL_REPR`] holding only the
    /// [`SyntaxKind::WITH_TOK`] — the marker the facade reads as the
    /// `Augmentation` kind (an `OBJECT_MODEL_REPR` with a `with` and no
    /// members). Caller has verified a raw `with` at the cursor.
    ///
    /// Returns `closed_block` (mirroring [`Self::parse_type_defn_repr`]): `true`
    /// iff the augment opened **and** closed its offside member block. The
    /// member-block items reuse [`Self::parse_member_block_items`]; the trailing
    /// `OBLOCKEND`/`ODECLEND` close virtuals are drained the same single-pair way
    /// the `= <repr>` path does, leaving any enclosing-body virtual to the
    /// caller's loop.
    fn parse_type_defn_augmentation(&mut self) -> bool {
        // The `Augmentation` marker: an empty `OBJECT_MODEL_REPR` carrying the
        // `with`. The members go to the outer slot, so the repr stays empty.
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::OBJECT_MODEL_REPR));
        self.bump_into(SyntaxKind::WITH_TOK);
        self.builder.finish_node(); // OBJECT_MODEL_REPR (empty: Augmentation marker)
        self.parse_with_augmentation_members(false, true)
    }

    /// Parse the body of a `with`-augmentation *after* the `with` keyword has
    /// already been emitted by the caller (as a [`SyntaxKind::WITH_TOK`], wrapped
    /// however the carrier needs): drain the opening `OBLOCKBEGIN`, run the
    /// member-block loop ([`Self::parse_member_block_items`], whose members become
    /// direct children of the *currently open* node — i.e. the carrier's **outer**
    /// members slot), then drain the member block's trailing close virtuals,
    /// leaving any enclosing-body separator to the caller's loop.
    ///
    /// Shared by the type augmentation ([`Self::parse_type_defn_augmentation`],
    /// phase 9.13a, FCS's `tyconDefnAugmentation`) and the exception augmentation
    /// ([`Self::parse_exception_defn`], phase 9.15b, FCS's `opt_classDefn = WITH
    /// classDefnBlock declEnd`): their filtered streams after the `with` are
    /// byte-for-byte identical (`WITH OBLOCKBEGIN classDefnMembers … ODECLEND`),
    /// so the offside member loop and the close drain are common. The two callers
    /// differ only in how they carry the `with` — the type wraps its `WITH_TOK`
    /// in an (otherwise empty) `OBJECT_MODEL_REPR` `Augmentation` marker, while
    /// the exception emits it as a plain direct child (FCS's
    /// `SynExceptionDefn.withKeyword`, with no repr).
    ///
    /// `sig` selects the member-item kind: `false` parses member *bodies*
    /// ([`Self::parse_member_block_items`]) for the impl carriers above; `true`
    /// parses member *sigs* ([`Self::parse_sig_member_block_items`]) for the
    /// signature exception augmentation (`opt_classSpfn`, phase 10.15) — the
    /// framing (`WITH OBLOCKBEGIN classSpfnBlock declEnd`) is identical.
    ///
    /// The `with`-block may be closed by an explicit `end` keyword
    /// (`type T with <members> end`, FCS's `classDefnBlock`/`opt_interfaceImplDefn`
    /// trailing `end`). In that offside position LexFilter emits the block's own
    /// `OBLOCKEND` and then relabels the `end` to `OEND` (`Virtual::End`) backed by
    /// the real `Token::End` at the same span. This consumes that `end` as an inert
    /// [`SyntaxKind::END_TOK`] child of the currently-open carrier node (FCS models
    /// no `end` slot, so the projection matches the offside-closed form), guarded so
    /// the object-expression callers' brace-backed `OEND` (a synthetic `Virtual::End`
    /// at a `}` with no raw `end`) is left for their own closer.
    ///
    /// `empty_block_takes_end` decides whether an *empty* `with`-block
    /// (`… with end`, no members) may still claim the explicit `end`. The type and
    /// exception augmentations pass `true` (FCS accepts an empty augmentation); the
    /// interface impl passes `false` (FCS *rejects* an empty `interface I with end`,
    /// so its `end` is left to the caller's stray-token recovery). Even when `true`,
    /// an empty block only claims an `end` that is *not offside* (see the close
    /// logic). A *non-empty* block always claims its `end` regardless of the flag.
    ///
    /// Returns `closed_block` (mirroring [`Self::parse_type_defn_repr`]): `true`
    /// iff the augment opened **and** closed its offside member block.
    pub(super) fn parse_with_augmentation_members(
        &mut self,
        sig: bool,
        empty_block_takes_end: bool,
    ) -> bool {
        // Opening `OBLOCKBEGIN` — consume as a zero-width ERROR (mirror
        // `parse_type_defn_repr`). Keep its span: for an *empty* block it is the
        // offside discriminant for a trailing explicit `end` (see the close below).
        let open_block_span = if let Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), sp)) =
            self.peek().cloned()
        {
            self.bump_into(SyntaxKind::ERROR);
            Some(sp)
        } else {
            None
        };
        let opened_block = open_block_span.is_some();

        // Members → outer slot (direct children of the open carrier node). An
        // augment with no members (`type T with` / `exception E with` then
        // nothing) just yields none, recovering. In a *signature* augment
        // (`opt_classSpfn`, phase 10.15) the members are member **sigs** (`member
        // M : int`), so route to the sig member-block items / start gate instead
        // of the impl member bodies.
        let pos_before_members = self.pos;
        if sig {
            if self.peek_is_sig_member_block_start() {
                self.parse_sig_member_block_items();
            }
            // Containment: `parse_sig_member_block_items` stops at any member-sig
            // kind this slice does not model (e.g. an attributed `inherit`/
            // `interface`, or a property get/set sig), leaving the cursor mid-block.
            // The close-drain below only fires at the block-closing `OBLOCKEND`, so
            // without this the rest of the augment would escape the carrier node and
            // be reprocessed as top-level specs. Skip the remainder (a no-op when
            // the block was fully consumed — cursor already at `OBLOCKEND`),
            // recording one diagnostic, so it stays inside the carrier node.
            if opened_block
                && !matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _)) | None
                )
            {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "this member signature in a `with` augmentation is not yet supported \
                              (later phase-10 slice)"
                        .to_string(),
                    span,
                });
                self.skip_to_enclosing_block_end();
            }
        } else if self.peek_is_object_model_start() {
            self.parse_member_block_items();
        }
        // Whether any member was consumed — the discriminant for claiming an
        // *empty* block's explicit `end` below.
        let had_members = self.pos > pos_before_members;

        // Close the augment. Both `tyconDefnAugmentation` and `opt_classDefn` are
        // `WITH classDefnBlock declEnd`, so the member block leaves *two* trailing
        // close virtuals: its body-closing `OBLOCKEND` and then the augment's own
        // `ODECLEND` (the `declEnd`). Consume both as zero-width ERRORs advancing
        // only `pos` (the `parse_type_defn_repr` body-close discipline — no raw
        // drain, so a following swallowed `type`/`and` that shares the virtual's
        // byte span, and the inter-definition trivia before an `and`, survive for
        // the `and`-chain loop). Draining the `declEnd` here is what lets a chained
        // `type T with … and U with …` reach its `AND_TOK` continuation, and lets
        // an augmented exception's sibling decl (`exception E with … ⏎ let y = …`)
        // reach the enclosing module loop — in both cases the cursor lands on the
        // continuation, not on the stranded `ODECLEND`. Any *further* virtual (a
        // second `ODECLEND` when nested in a module body, then the body's
        // `OBLOCKSEP`) is left for the caller's loop.
        let closed_block = opened_block
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
            );
        if closed_block {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }
        // Optional explicit `end` closer (`… with <members> end`): after the member
        // block's `OBLOCKEND`, LexFilter surfaces the `end` keyword as an `OEND`
        // (`Virtual::End`) backed by the real `Token::End` at the same span (there
        // is no `ODECLEND` in this form — the `end` stands in for it). Emit it as an
        // inert `END_TOK` child of the open carrier node the `OWITH`-keyword way
        // (`drain_raw_up_to` + `emit_text` + advance both cursors — a zero-width
        // `bump_into` would orphan the "end" text and break `text(tree) == source`).
        //
        // Guard on a *real* `Token::End` at the matching span so the
        // object-expression callers' brace-backed synthetic `OEND` (a `}` with no
        // raw `end`) is left for their own closer. A *non-empty* block always claims
        // its `end`. An *empty* block claims it only when the carrier allows one
        // (`empty_block_takes_end` — type/exception yes, interface no) **and** the
        // `end` is not offside: FCS rejects `type T with⏎end` when the `end` sits at
        // or left of the declaration column (an FS offside error) but accepts the
        // same-line (`type T with end`) and indented (`type T with⏎  end`) forms.
        // LexFilter encodes exactly that: for a validly-placed empty closer it opens
        // the block *at the `end`* (so `OBLOCKBEGIN` and `OEND` share the `end`'s
        // span), whereas an offside `end` opens the block back at the `with` (an
        // earlier span). So an empty block claims its `end` iff the opening
        // `OBLOCKBEGIN` coincides with this `OEND`; an offside `end` is left to the
        // caller's stray-token recovery, matching FCS's rejection.
        if let Some((Ok(FilteredToken::Virtual(Virtual::End)), end_span)) = self.peek().cloned()
            && matches!(
                self.next_non_trivia_raw_at_pos_with_span(),
                Some((Token::End, raw_span)) if raw_span == end_span,
            )
            && (had_members
                || (empty_block_takes_end && open_block_span.as_ref() == Some(&end_span)))
        {
            self.drain_raw_up_to(end_span.start);
            self.emit_text(SyntaxKind::END_TOK, end_span);
            self.raw_pos += 1;
            self.pos += 1;
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::DeclEnd)), _))
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }
        closed_block
    }

    /// Parse the value-binding block of an object expression —
    /// `{ new T() with X = e [and Y = e2 …] }` (FCS's `objExprBindings: OWITH
    /// localBindings OEND`). The caller ([`Self::parse_obj_or_computation_brace`])
    /// has verified the cursor is at the `OWITH` ([`Virtual::With`]) following the
    /// base call; this emits the `with`, the `localBindings` as [`SyntaxKind::BINDING`]
    /// children of the *currently open* `OBJ_EXPR` (FCS's `bindings` slot, distinct
    /// from the member form's [`SyntaxKind::MEMBER_DEFN`] children and the
    /// [`SyntaxKind::INTERFACE_IMPL`] extra-impls), then consumes the closing
    /// `OEND` ([`Virtual::End`]).
    ///
    /// The `with` is the `Virtual::With` (`OWITH`, the `WithAsLet` context) relabel
    /// — emitted as `WITH_TOK` from its raw `Token::With` backing, the same way the
    /// record copy-update `{ e with F = v }` emits its `OWITH`. The bindings reuse
    /// the shared [`Self::parse_binding`] / `and`-chain machinery (FCS's
    /// `localBindings`); unlike the hardwhite `let` block they carry no per-binding
    /// `OBLOCKBEGIN`/`OBLOCKEND`, so each `and` follows the prior RHS directly. The
    /// head's leading keyword is FCS's `SynLeadingKeyword.Synthetic` and the rest
    /// `And`, supplied by the normaliser (there is no per-binding keyword token).
    ///
    /// A head `mutable` (`{ new T() with mutable X = e }`) never reaches here: the
    /// LexFilter lexes `with mutable` as the *member* form (a raw `Token::With`
    /// opening an `OBLOCKBEGIN`, not the value form's `OWITH`), so it is routed to
    /// the member branch, where `mutable` is not a valid member start — matching
    /// FCS's "Unexpected keyword 'mutable' in object expression" error and its empty
    /// `bindings`. (`inline X = e`, `inline mutable X = e`, and a non-head
    /// `and mutable Y = e` all *do* lex as `OWITH` and parse here.)
    pub(super) fn parse_obj_expr_value_bindings(&mut self) {
        // Emit the `OWITH` `with` from its raw `Token::With` backing.
        if let Some((Ok(FilteredToken::Virtual(Virtual::With)), with_span)) = self.peek().cloned() {
            self.drain_raw_up_to(with_span.start);
            self.emit_text(SyntaxKind::WITH_TOK, with_span);
            self.raw_pos += 1;
            self.pos += 1;
        }

        // The head binding, then any `and`-chained continuations. The `and` is
        // matched as the *immediate* next raw token — deliberately **not** after a
        // `Virtual::BlockEnd` drain. In the `OWITH localBindings` block a binding's
        // RHS that opens an offside block has its `OBLOCKEND` drained *inside*
        // `parse_binding` (the shared `parse_let_equals_rhs` machinery), so a
        // well-formed continuation (`X = 1⏎ and Y = 2`) leaves the cursor directly
        // on the raw `and`. Draining a `BlockEnd` here before testing for `and`
        // would instead fold an FCS-rejected form — `X = if p then 1 else 2 and Y
        // = 3` (FCS errors at the `and`, since a control-flow RHS does not admit a
        // trailing `and`) — into two bindings, accepting input FCS rejects.
        self.parse_binding();
        while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::And)), _))) {
            self.bump_into(SyntaxKind::AND_TOK);
            self.parse_binding();
        }

        // Consume the closing `OEND` (`Virtual::End`) as a zero-width ERROR — the
        // raw `}` it shadows is recovered by the caller's `bump_swallowed_closer`,
        // and the brace's `BlockEnd`/`DeclEnd` are left for the enclosing loop. As
        // with the `and` check above, a binding RHS's own offside `OBLOCKEND` is
        // already drained inside `parse_binding`, so the cursor reaches the `OEND`
        // directly (verified by the control-flow / offside-RHS diff tests).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::End)), _))
        ) {
            self.builder
                .token(FSharpLang::kind_to_raw(SyntaxKind::ERROR), "");
            self.pos += 1;
        }
    }
}
