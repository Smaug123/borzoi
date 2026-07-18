//! Object-model type-body productions: the member-block loop and its items —
//! members, `val` fields, auto-properties, explicit constructors, abstract
//! slots, `inherit` / `interface` clauses, get/set accessors, and
//! dotted-operator member heads. Also hosts the top-level `module` / `open`
//! decl dispatch ([`Parser::parse_module_decl`] / [`Parser::parse_open_decl`]).
//!
//! The dotted-`opName` head itself (`member x.(+)`, `member A.B.(|Foo|Bar|)`) is
//! not member-specific — it is FCS's `pathOp` ending in an `opName`, shared with
//! every pattern head — so its lookahead/emitter live in [`super::pat`]
//! ([`Parser::peek_dotted_opname_pat_head`], [`Parser::open_dotted_opname_pat_head`]).

use super::*;

impl<'src> Parser<'src> {
    /// `true` iff the type-definition body (positioned just after the opening
    /// `OBLOCKBEGIN`) begins an *object model* — i.e. the cursor is at a real
    /// `member` keyword (phase 9.7, a genuine `Token::Member` LexFilter passes
    /// through), a `static member` (phase 9.9a, a `Token::Static` immediately
    /// before a `member`), or a class-local `let`/`let rec` (phase 9.8b, a
    /// `Virtual::Let`) — through to the `val`/`new`/`abstract`/`inherit`/
    /// `interface` openers of later 9.x slices. The `do`-binding is still a later
    /// phase-9 slice.
    ///
    /// A leading `[<…>]` (phase 10.7f) also starts an object model: in repr/member
    /// position it can only be an attributed member, of *any* kind — including the
    /// virtual-only class-local `let`/`interface` forms the raw classifier can't
    /// see — so it is recognised directly here. [`Self::parse_member_block_items`]
    /// then consumes the attributes and classifies the underlying item.
    pub(super) fn peek_is_object_model_start(&self) -> bool {
        self.classify_object_model_item().is_some()
            || matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
            )
    }

    /// Classify the object-model body item at the cursor (phases 9.7–9.9b), or
    /// `None` if the cursor isn't at a supported item. Drives both the repr
    /// dispatch ([`Self::parse_type_defn_repr`]) and the member-block loop.
    ///
    /// A class-local `let` is a cheap filtered peek (`Virtual::Let`); the rest
    /// are classified by the first ≤3 *significant* raw tokens at the cursor
    /// (scanned from `raw_pos`, O(small) — an O(n) scan here would be O(n²)
    /// across a file's type definitions). `[static] member val …` is an
    /// auto-property (9.9c), distinguished from a `[static] member …` method by
    /// the `val` after `member`, and from a `[static] val …` field (9.9b) by the
    /// leading `member`.
    pub(super) fn classify_object_model_item(&self) -> Option<ObjectModelItem> {
        match self.peek() {
            // A class-local `let`.
            Some((Ok(FilteredToken::Virtual(Virtual::Let)), _)) => {
                return Some(ObjectModelItem::ClassLet);
            }
            // An `interface I [with …]` member (phase 9.11b) — the `interface`
            // keyword in member position is relabelled to `OINTERFACE_MEMBER`
            // (`Virtual::InterfaceMember`), so it is classified here at the
            // virtual level (like the class-local `let`), not in the raw scan.
            // Must precede the catch-all virtual arm below.
            Some((Ok(FilteredToken::Virtual(Virtual::InterfaceMember)), _)) => {
                return Some(ObjectModelItem::Interface);
            }
            // A class-body `do <expr>` binding (phase 9.8d). LexFilter rewrites
            // the raw `Token::Do` to `Virtual::Do` (the same relabel as a
            // statement-level `do`), so it is classified here at the virtual
            // level, like the class-local `let`. Must precede the catch-all
            // virtual arm below (a `Virtual::Do` *is* an item, not a close).
            // (`static do` opens with a raw `Token::Static`, so it is reached by
            // the raw scan, not here.)
            Some((Ok(FilteredToken::Virtual(Virtual::Do)), _)) => {
                return Some(ObjectModelItem::Do);
            }
            // Any *other* pending layout virtual (a body-close `OBLOCKEND`, an
            // `OBLOCKSEP`/`ODECLEND`, …) is NOT an item: the raw scan below is
            // filtered-virtual-blind and would "see through" the close to a later
            // raw `member`/`val` (e.g. a col-0 `member` after the body), claiming
            // it and consuming the `OBLOCKEND` as a phantom keyword. Stop here so
            // the loop ends and the caller handles the close.
            Some((Ok(FilteredToken::Virtual(_)), _)) => return None,
            _ => {}
        }
        self.classify_object_model_item_from_raw()
    }

    /// The raw-stream half of [`Self::classify_object_model_item`]: classify
    /// the member-block item by the first ≤3 significant raw tokens at the
    /// cursor, with no filtered-virtual guard. Split out so the bare-trailing-
    /// members gate (phase 9.13b, [`Self::bare_trailing_member_follows`]) can
    /// classify the item *behind* the `OBLOCKSEP` at the cursor — the
    /// separator is a layout virtual with no raw counterpart, so the raw scan
    /// already starts at the item's first token.
    ///
    /// A leading attribute-list run (`[<A>] member …`, phase 10.7f) is skipped
    /// before classifying, so the bare-trailing-member gate
    /// ([`Self::bare_trailing_member_follows`]) recognises an attributed member.
    /// The first `>]` closes each list — adequate for this classification
    /// lookahead (the real attribute parse runs in
    /// [`Self::parse_member_block_items`]). A *virtual-only* attributed item
    /// (class-local `[<A>] let` / `[<A>] interface`) is not classifiable from the
    /// raw stream — the entry gate [`Self::peek_is_object_model_start`] catches
    /// those via the leading `[<` on the filtered cursor instead.
    /// `true` if `tok` (the token in name position after an `abstract [member]
    /// [access] [inline]` run) and the following raw tokens from `sig` form a
    /// valid abstract-slot *name* — an identifier / quoted identifier, the glued
    /// `(*)` operator-value, or a parenthesised operator (`(+)`) or active-pattern
    /// (`(|Foo|_|)`) name. Mirrors the member-sig start gate's name check
    /// (`peek_is_member_sig_start`) so the classifier and [`Self::parse_abstract_
    /// slot_at`] stay in lockstep: an over-accept routes an unparseable form into
    /// the slot parser, an under-accept leaves a valid one on the error path.
    fn raw_is_abstract_slot_name<'a>(
        tok: Option<&'a Token<'src>>,
        sig: &mut impl Iterator<Item = &'a Token<'src>>,
    ) -> bool
    where
        'src: 'a,
    {
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

    pub(super) fn classify_object_model_item_from_raw(&self) -> Option<ObjectModelItem> {
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
            // `member val …` — an auto-property (9.9c); else `member …` a method.
            Some(Token::Member) => Some(match sig.next() {
                Some(Token::Val) => ObjectModelItem::AutoProperty,
                _ => ObjectModelItem::Member,
            }),
            // `override …` / `default …` (9.10a). `override val …` / `default
            // val …` is the *auto-property* production (`memberFlags
            // autoPropsDefnDecl`, `pars.fsy:2099`, leading keyword
            // `OverrideVal`/`DefaultVal`), exactly like `member val`; a bare
            // `override`/`default` is a `MEMBER_DEFN` whose binding carries the
            // `Override`/`Default` leading keyword (no `member` kw).
            Some(Token::Override | Token::Default) => Some(match sig.next() {
                Some(Token::Val) => ObjectModelItem::AutoProperty,
                _ => ObjectModelItem::Member,
            }),
            Some(Token::Static) => match sig.next() {
                // `static member val …` — a static auto-property; else a method.
                Some(Token::Member) => Some(match sig.next() {
                    Some(Token::Val) => ObjectModelItem::AutoProperty,
                    _ => ObjectModelItem::Member,
                }),
                // `static val …` — a static field (9.9b).
                Some(Token::Val) => Some(ObjectModelItem::ValField),
                // `static let`/`static use` — a static class-local binding (9.8c,
                // FCS's `STATIC classDefnBindings`, `pars.fsy:2009`). The
                // LexFilter has already swapped the `CtxtMemberHead` `static`
                // opened for a `CtxtLetDecl` and relabelled the `let`/`use` to a
                // `Virtual::Let` (`lexfilter/pushes.rs`), so only the leading raw
                // `static` distinguishes this from a `ClassLet`.
                Some(Token::Let | Token::Use) => Some(ObjectModelItem::StaticClassLet),
                // `static do` — FCS's `StaticDo` (phase 9.8d). The `do` is the
                // raw `Token::Do` backing the `Virtual::Do` (the LexFilter relabel
                // keeps the raw token), so the raw scan sees it here. (A plain
                // `do` opens with the `Virtual::Do` and is caught at the virtual
                // level in `classify_object_model_item`.)
                Some(Token::Do) => Some(ObjectModelItem::StaticDo),
                // `static abstract [member] [access] [inline] <ident>` — a
                // static-abstract interface slot (F# 7 IWSAM). Same shape as the
                // bare `abstract` arm below, one keyword deeper; `parse_abstract_
                // slot_at` consumes the leading `static`. Only `static abstract`
                // (this order) is legal — `abstract static` is an FCS error and
                // stops cleanly (the bare `Abstract` arm never sees a `static`).
                Some(Token::Abstract) => {
                    let mut tok = sig.next();
                    if matches!(tok, Some(Token::Member)) {
                        tok = sig.next();
                    }
                    while matches!(
                        tok,
                        Some(Token::Private | Token::Internal | Token::Public | Token::Inline)
                    ) {
                        tok = sig.next();
                    }
                    if Self::raw_is_abstract_slot_name(tok, &mut sig) {
                        Some(ObjectModelItem::AbstractSlot)
                    } else {
                        None
                    }
                }
                _ => None,
            },
            // `val …` — a field (9.9b).
            Some(Token::Val) => Some(ObjectModelItem::ValField),
            // `abstract [member] <nameop> : …` — an abstract slot (9.10c). The
            // `nameop` is an identifier, a parenthesised operator (`abstract (+) :
            // …`), or an active-pattern name (`abstract (|Foo|_|) : …`) — the same
            // name surface the `val`-sig / member-sig / member-def heads and the
            // `let (+)` / `let (|Foo|_|)` binding head accept. `parse_abstract_
            // slot_at` routes all three through the binding-head machinery, so the
            // classifier claims all three (kept in lockstep via the shared
            // [`Self::raw_is_abstract_slot_name`]).
            Some(Token::Abstract) => {
                // `abstract [member] [access] [inline] <nameop>` (FCS's
                // `abstractMemberFlags opt_access opt_inline nameop`,
                // `pars.fsy:2060`). The `opt_access` is *illegal* (abstract slots
                // inherit the type's visibility), but FCS still recovers an
                // `AbstractSlot` + a diagnostic, so we claim it and the slot parser
                // reports the error (consuming `access`/`inline` in either order is
                // harmless — both are elided).
                let mut tok = sig.next();
                if matches!(tok, Some(Token::Member)) {
                    tok = sig.next();
                }
                while matches!(
                    tok,
                    Some(Token::Private | Token::Internal | Token::Public | Token::Inline)
                ) {
                    tok = sig.next();
                }
                if Self::raw_is_abstract_slot_name(tok, &mut sig) {
                    Some(ObjectModelItem::AbstractSlot)
                } else {
                    None
                }
            }
            // `new(…) = …` — an explicit constructor (9.10b), optionally access-
            // modified (`private new(…)` — FCS's `opt_access` before `NEW`,
            // `pars.fsy:2106`, accepted with no diagnostic). A leading access
            // modifier before any *other* member form is a clean stop (None), as
            // before.
            Some(Token::New) => Some(ObjectModelItem::NewCtor),
            Some(Token::Private | Token::Internal | Token::Public)
                if matches!(sig.next(), Some(Token::New)) =>
            {
                Some(ObjectModelItem::NewCtor)
            }
            // `inherit Base[(args)] [as base]` — a base-class clause (9.11a).
            // `inherit` is a plain filtered keyword (LexFilter passes it
            // through, pushing the silent `CtxtMemberHead`).
            Some(Token::Inherit) => Some(ObjectModelItem::Inherit),
            // A class-local `let`/`use` whose leading `[<…>]` attribute run was
            // already consumed (the *same-line* `[<A>] let …` form, phase 10.7l):
            // once `parse_attribute_lists` has eaten the run, the `let` arrives as
            // a *raw* `Token::Let`/`Use` rather than the LexFilter `Virtual::Let`
            // the unattributed form opens with (caught earlier in
            // [`Self::classify_object_model_item`]). Claim it here so the member
            // loop wraps the attribute lists into the binding. (`static let` keeps
            // its leading `Token::Static`, handled by the `Static` arm above.)
            Some(Token::Let | Token::Use) => Some(ObjectModelItem::ClassLet),
            _ => None,
        }
    }

    /// Iterator over the *significant* raw tokens at/after the cursor — the same
    /// scan as [`Self::next_non_trivia_raw_at_pos`] (skips trivia and any
    /// already-consumed prefix), but yields successive tokens for a short
    /// fixed-length lookahead. Starts from `raw_pos`, so taking the first few
    /// tokens is O(small) rather than O(file).
    pub(super) fn significant_raw_from_cursor(&self) -> impl Iterator<Item = &Token<'src>> {
        let consumed = self.raw_consumed_end;
        self.raw_tokens
            .iter()
            .skip(self.raw_pos)
            .filter(move |(_, span)| span.start >= consumed)
            .filter_map(|(res, _)| res.as_ref().ok())
            .filter_map(raw_significant)
    }

    /// Parse an object-model repr (`member …` block, phase 9.7) into a
    /// [`SyntaxKind::OBJECT_MODEL_REPR`] node — `SynTypeDefnRepr.ObjectModel(
    /// Unspecified, members, _)` (`pars.fsy:1812`). The caller
    /// ([`Self::parse_type_defn_repr`]) has already consumed the body-opening
    /// `OBLOCKBEGIN`, so the cursor sits on the first `member` keyword.
    ///
    /// Each item is parsed by its production
    /// ([`Self::parse_member_defn_at`] / [`Self::parse_let_decl_at`] /
    /// [`Self::parse_val_field_at`]); [`Self::consume_object_model_item_terminator`]
    /// then consumes that item's trailing virtuals up to the next item, leaving
    /// the type body's closing `OBLOCKEND` for [`Self::parse_type_defn_repr`]'s
    /// `closed_block` handling. The loop ends when the next position is not a
    /// member-block item (a body-close `OBLOCKEND`, or anything else).
    pub(super) fn parse_object_model_repr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::OBJECT_MODEL_REPR));
        self.parse_member_block_items();
        self.builder.finish_node(); // OBJECT_MODEL_REPR
    }

    /// `true` iff the type-definition body (positioned just after the opening
    /// `OBLOCKBEGIN`) is an explicit `class`/`struct`/`interface … end` kind
    /// marker (phase 9.12) — the cursor is at a raw `Token::Class`/`Token::Struct`/
    /// `Token::Interface`. The member-form `interface` (`interface I with …`,
    /// 9.11b) arrives as `Virtual::InterfaceMember`, not a raw `Token::Interface`,
    /// so it routes through the object-model arm instead and does not collide.
    ///
    /// `struct` is also the head of two *type* forms — the struct tuple
    /// `struct (T * U)` (`atomType` STRUCT-LPAREN, `pars.fsy:6549`) and the struct
    /// anon-record `struct {| … |}` (phase 7.9) — which are abbreviations, not kind
    /// markers, and must reach [`Self::parse_type`]. A struct *kind marker* body
    /// instead opens with a member (`val`/`member`/…) or `end`, never `(` or `{|`,
    /// so exclude those two lookaheads. (`class`/`interface` head no type form.)
    pub(super) fn peek_is_kind_marked_repr_start(&self) -> bool {
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Class | Token::Interface)), _)) => true,
            Some((Ok(FilteredToken::Raw(Token::Struct)), _)) => !matches!(
                self.nth_significant_raw_at_pos(1),
                Some(Token::LParen | Token::LBraceBar)
            ),
            _ => false,
        }
    }

    /// Parse an explicit-kind-marked repr `class … end` / `struct … end` /
    /// `interface … end` (phase 9.12) into a [`SyntaxKind::OBJECT_MODEL_REPR`] —
    /// `SynTypeDefnRepr.ObjectModel(Class|Struct|Interface, members, _)` (grammar
    /// `tyconClassDefn`/`classOrInterfaceOrStruct`, `pars.fsy:1798`/`:2528`). The
    /// caller ([`Self::parse_type_defn_repr`]) has already consumed the
    /// body-opening `OBLOCKBEGIN`, so the cursor sits on the kind keyword.
    ///
    /// The kind keyword is bumped as a `CLASS_TOK`/`STRUCT_TOK`/`INTERFACE_TOK`
    /// direct child (the facade reads the kind off it; not confused with 9.11b's
    /// interface *member*, whose `INTERFACE_TOK` nests in an `INTERFACE_IMPL`
    /// node). `class`/`interface` (FCS `AddBlockEnd::Yes`) then open one inner
    /// `OBLOCKBEGIN`; `struct` (`AddBlockEnd::No`) opens none — consume one if
    /// present. Members reuse [`Self::parse_member_block_items`] (the explicit
    /// `end` suppresses the offside member-close virtuals, which the lenient
    /// terminator tolerates; the loop stops at the raw `end`). The `end` keyword
    /// is bumped as `END_TOK`; the trailing layout virtuals after it are left for
    /// the enclosing loop (elided, drained as strays — like other nested reprs).
    pub(super) fn parse_kind_marked_repr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::OBJECT_MODEL_REPR));
        // The kind keyword → the marker token the facade keys the kind off.
        let kw = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Class)), _)) => SyntaxKind::CLASS_TOK,
            Some((Ok(FilteredToken::Raw(Token::Struct)), _)) => SyntaxKind::STRUCT_TOK,
            _ => SyntaxKind::INTERFACE_TOK,
        };
        self.bump_into(kw);
        // The inner member-block `OBLOCKBEGIN` (FCS `AddBlockEnd::Yes` for
        // `class`/`interface`; `struct` has none) — consume one if present.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
        // Members (9.7–9.11); the loop stops at the raw `end` (not a member item).
        self.parse_member_block_items();
        // The inner member block's close `OBLOCKEND` (FCS's public token dump
        // swallows it, but our real stream carries `Virtual::BlockEnd` — see the
        // harness note in `tests/all/common/mod.rs`). `class`/`interface`
        // (`AddBlockEnd::Yes`) emit one here before the `end`; `struct`
        // (`AddBlockEnd::No`) emits none. Drain a run as zero-width `ERROR`s; the
        // outer body block's close sits *after* `end`, so this never over-reaches.
        while matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
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

    /// `true` iff the type-definition body (positioned just after the opening
    /// `OBLOCKBEGIN`) is a delegate body — the cursor is at a raw
    /// `Token::Delegate` (`type T = delegate of int -> int`, `pars.fsy:1779`).
    /// `delegate` is not a `typ`-starter, so the abbreviation arm never claims
    /// it; the generic-typar-constraint `delegate<…>` form (`'a : delegate<…>`)
    /// is a typar-constraint slice, never a type-defn body, so there is no
    /// collision here.
    pub(super) fn peek_is_delegate_repr_start(&self) -> bool {
        matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Delegate)), _))
        )
    }

    /// Parse a delegate body — `delegate of <topType>` (`pars.fsy:1779`) — into a
    /// [`SyntaxKind::DELEGATE_REPR`]. The caller ([`Self::parse_type_defn_repr`])
    /// has already consumed the body-opening `OBLOCKBEGIN`, so the cursor sits
    /// on the `delegate` keyword.
    ///
    /// FCS lowers this to `SynTypeDefnRepr.ObjectModel(SynTypeDefnKind.Delegate(
    /// ty, arity), [AbstractSlot "Invoke"], _)`; we keep the surface shape
    /// `[DELEGATE_TOK, OF_TOK, <type>]`, parsing the signature `ty` with the
    /// shared [`Self::parse_type`]. FCS's `topType` differs from `typ` only by
    /// its arity tracking (reflected anyway in the `Tuple`/`Fun` structure of
    /// the type) and by accepting argument labels / optional args (`x:int`,
    /// `?x:int`) — niche `topType`-only forms left for a later slice. The
    /// body-closing `OBLOCKEND` is left for the caller's `closed_block`
    /// handling, exactly like the abbreviation arm.
    pub(super) fn parse_delegate_repr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::DELEGATE_REPR));
        // The `delegate` keyword.
        self.bump_into(SyntaxKind::DELEGATE_TOK);
        // `of` — required by the grammar (`DELEGATE OF topType`). Record a clean
        // error if absent; the type parse below still runs for recovery.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Of)), _))) {
            self.bump_into(SyntaxKind::OF_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `of` after `delegate`".to_string(),
                span,
            });
        }
        // The signature type — FCS's `DELEGATE OF topType`, so labelled parameters
        // (`delegate of x: int -> int`, phase 10.12b) are admitted. `parse_top_type`
        // self-guards and self-drains, so a missing / malformed type records a clean
        // error rather than panicking. (Identical to `parse_type` for an unnamed
        // signature, so impl-side delegates are unaffected.)
        self.parse_top_type();
        self.builder.finish_node(); // DELEGATE_REPR
    }

    /// `true` when the type-definition body opens FSharp.Core's inline-IL form
    /// `( # "instr" # )` (`SynTypeDefnSimpleRepr.LibraryOnlyILAssembly`,
    /// `pars.fsy:2483`). Must be checked before the abbreviation arm
    /// ([`Self::peek_starts_type`]), which would otherwise claim the `(` as a
    /// paren-type start and choke on the `#` (the bug this fixes).
    ///
    /// In *type* position `( #` alone is ambiguous: it also opens a
    /// parenthesised flexible type `(#ty)` (`type T = (#int)`, an ordinary
    /// abbreviation to a `#`-constraint type). Inline IL is distinguished by the
    /// instruction *string* after the `#`, so this requires the full
    /// `( # <string>` prefix — `(` then `#` then a string literal — on the raw
    /// stream. (The expression-side `(#` lookahead in
    /// [`Self::parse_atomic_expr_head`] needs no such check: `(#ty)` is not an
    /// expression, so a `#` after `(` is unambiguously inline IL there.)
    pub(super) fn peek_is_inline_il_repr_start(&self) -> bool {
        let mut sig = self.significant_raw_from_cursor();
        matches!(sig.next(), Some(Token::LParen))
            && matches!(sig.next(), Some(Token::Hash))
            && matches!(
                sig.next(),
                Some(Token::String | Token::VerbatimString | Token::TripleString)
            )
    }

    /// Parse FSharp.Core's inline-IL type body `( # "instr" # )`
    /// (`SynTypeDefnSimpleRepr.LibraryOnlyILAssembly`, `pars.fsy:2483`'s
    /// `LPAREN HASH string HASH rparen`) into a [`SyntaxKind::INLINE_IL_REPR`].
    /// The caller ([`Self::parse_type_defn_repr`]) has consumed the body-opening
    /// `OBLOCKBEGIN` and verified [`Self::peek_is_inline_il_repr_start`], so the
    /// cursor sits on the `(`.
    ///
    /// Unlike the expression-position inline IL — which FCS reaches through
    /// `parenExpr` and so wraps in a `Paren(LibraryOnlyILAssembly)` with the
    /// `(`/`)` owned by an outer [`SyntaxKind::PAREN_EXPR`] — FCS's
    /// type-definition grammar consumes the surrounding `(`/`)` directly into
    /// this simple repr, so they are children of `INLINE_IL_REPR`, not a
    /// wrapper. The closing `)` is LexFilter-swallowed (every paren closer is)
    /// and recovered from the raw stream via [`Self::bump_swallowed_rparen`].
    /// The type form takes only the instruction string — no type/value
    /// arguments and no return type (those are the expression form alone) — so
    /// the body is `[LPAREN_TOK, HASH_TOK, <il-string>, HASH_TOK, RPAREN_TOK]`.
    /// The instruction string and the layout-skip between tokens reuse the
    /// expression form's [`Self::parse_il_instruction_string`] /
    /// [`Self::skip_inline_il_layout`].
    pub(super) fn parse_inline_il_repr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INLINE_IL_REPR));
        self.bump_into(SyntaxKind::LPAREN_TOK);
        // The opening `#` — guaranteed by `peek_is_inline_il_repr_start` to be
        // the next significant raw token, so it is a real filtered token here
        // (any `(`–`#` whitespace is drained as its leading trivia).
        self.skip_inline_il_layout();
        self.bump_into(SyntaxKind::HASH_TOK);
        self.skip_inline_il_layout();
        self.parse_il_instruction_string();

        // The closing `#`, then the LexFilter-swallowed `)`. Guard the `#`
        // against the swallowed closer (a missing `#`, `( # "x" )`) so a stray
        // `#` past the `)` is never consumed — the same discipline as the
        // expression form.
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
                message: "expected `#)` to close the inline-IL type definition".to_string(),
                span,
            });
        }
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
        self.builder.finish_node(); // INLINE_IL_REPR
    }

    /// Parse the items of a member block — one or more `member …` / class-local
    /// `let` items — into the **current** open node, consuming each item's own
    /// RHS-close `OBLOCKEND` and the inter-item `ODECLEND`/`OBLOCKSEP`
    /// separators, and leaving the body-closing `OBLOCKEND` for the caller.
    /// Shared by the pure object-model repr ([`Self::parse_object_model_repr`],
    /// which wraps these in an `OBJECT_MODEL_REPR`), the augmentation form
    /// ([`Self::parse_type_defn_augmentation`], phase 9.13a), and the
    /// bare-trailing-members hook in [`Self::parse_type_defn_repr`] (phase
    /// 9.13b) — the latter two route the members into the outer
    /// `SynTypeDefn.members` slot. Caller has verified
    /// [`Self::peek_is_object_model_start`].
    pub(super) fn parse_member_block_items(&mut self) {
        loop {
            // Leading attribute lists on a member (phase 10.7f). In repr/member
            // position a leading `[<` can only be an attributed member, so parse
            // the lists under a checkpoint and attach them to the member node (the
            // `MEMBER_DEFN`/`GET_SET_MEMBER` is opened at `cp`, so the attributes
            // become its leading children — FCS homes them in
            // `SynBinding.attributes`). The offside `[<A>]⏎member …` layout leaves
            // a `BlockSep` before the (real-token) member keyword; skip it.
            let member_cp = if let Some((Ok(FilteredToken::Raw(Token::LBrackLess)), span)) =
                self.peek().cloned()
            {
                self.drain_raw_up_to(span.start);
                let cp = self.builder.checkpoint();
                self.parse_attribute_lists();
                // `member`/`new`/… are real filtered tokens (not LexFilter-
                // swallowed), so a plain `bump_into` is safe — no swallowed
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
            let item = self.classify_object_model_item();
            // Attributes attach to a `member`/`new` (10.7f), abstract slot (10.7g),
            // auto-property (10.7h), `val` field (10.7i), or a class-local
            // `let`/`use`/`static let` binding (10.7l, `SynBinding.attributes` on
            // the head binding); on any other carrier (inherit / interface) they
            // are a later slice — flag and leave the parsed `ATTRIBUTE_LIST`s as
            // bare siblings, then parse the item itself so the rest of the block
            // still parses.
            if member_cp.is_some()
                && !matches!(
                    item,
                    Some(
                        ObjectModelItem::Member
                            | ObjectModelItem::NewCtor
                            | ObjectModelItem::AbstractSlot
                            | ObjectModelItem::AutoProperty
                            | ObjectModelItem::ValField
                            | ObjectModelItem::ClassLet
                            | ObjectModelItem::StaticClassLet
                    )
                )
            {
                let span = self
                    .next_non_trivia_raw_at_pos_with_span()
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "attributes on this member are a later phase-10.7 slice".to_string(),
                    span: span.clone(),
                });
                // `parse_attribute_lists` left `raw_pos` on the trivia after the
                // final `>]`; drain it onto the carrier keyword so the deferred
                // item parser sees a clean cursor (e.g. `parse_interface_member`
                // asserts `raw_pos` is *at* its keyword). The `member`/`new` parsers
                // drain leading trivia themselves, so this is only needed on the
                // deferred path.
                self.drain_raw_up_to(span.start);
            }
            // A member/`let` has an offside `= <expr>` RHS block (leaving a
            // RHS-close `OBLOCKEND`); a `val` field has none.
            let has_rhs_block = match item {
                Some(ObjectModelItem::Member) => {
                    // A regular `member …` has an offside `= <expr>` RHS block; a
                    // get/set property (9.14) instead consumes its own `OEND`
                    // close, so it takes the no-RHS-block terminator (like a `val`
                    // field). `parse_member_defn_at` returns which form it parsed.
                    !self.parse_member_defn_at(member_cp)
                }
                Some(ObjectModelItem::ClassLet) => {
                    // Class-local `let`/`let rec` (phase 9.8b) — same grammar as a
                    // module-level `let`, into a `MEMBER_LET_BINDINGS` node. A
                    // leading `[<…>]` attribute run (phase 10.7l) is threaded via
                    // `member_cp` so the lists wrap into the node as leading
                    // children of the head binding (`SynBinding.attributes`).
                    self.parse_let_decl_at(member_cp, SyntaxKind::MEMBER_LET_BINDINGS);
                    true
                }
                Some(ObjectModelItem::StaticClassLet) => {
                    // `static let`/`static let rec` (phase 9.8c, FCS's `STATIC
                    // classDefnBindings`, `pars.fsy:2009`) — the same
                    // `MEMBER_LET_BINDINGS` as a `ClassLet` with a leading
                    // `STATIC_TOK`. Bump the raw `static` under a checkpoint so
                    // `parse_let_decl_at`'s `Some(cp)` arm reopens the node *at*
                    // the checkpoint, retroactively nesting the `STATIC_TOK`
                    // inside it (and draining the `static`→`let` trivia after it).
                    // A leading `[<…>]` run (phase 10.7l, FCS's `opt_attributes`
                    // *before* `STATIC`) wraps in too: reuse `member_cp` as the
                    // checkpoint so the lists become leading children ahead of
                    // `STATIC_TOK`.
                    let cp = member_cp.unwrap_or_else(|| self.builder.checkpoint());
                    self.bump_into(SyntaxKind::STATIC_TOK);
                    // The `let`/`use` must *immediately* follow `static` — the
                    // LexFilter swaps `static`'s `CtxtMemberHead` for a
                    // `CtxtLetDecl` and relabels the keyword to `Virtual::Let`
                    // only when they are adjacent. A layout break in between
                    // (`static`⏎`let`, which leaves an `OBLOCKSEP` before the
                    // `Virtual::Let`) is FCS's "Incomplete structured construct"
                    // error; the raw classifier (virtual-blind) cannot see the
                    // separator, so guard here: record an error and recover with a
                    // bare `STATIC_TOK`-only node, leaving the trailing `let` to
                    // re-classify as a plain class-local `let` next iteration.
                    // (Mirrors the `static`⏎`member`/`val` recovery, which error
                    // rather than parse a member.) Without the guard,
                    // `parse_let_decl_at`'s let-at-cursor invariant would trip.
                    if matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::Let)), _))
                    ) {
                        // After the bump the cursor sits on the `Virtual::Let`, so
                        // the binding parse proceeds exactly as the non-static form.
                        self.parse_let_decl_at(Some(cp), SyntaxKind::MEMBER_LET_BINDINGS);
                        true
                    } else {
                        let span = self
                            .next_non_trivia_raw_at_pos_with_span()
                            .map(|(_, s)| s)
                            .unwrap_or_else(|| self.source.len()..self.source.len());
                        self.errors.push(ParseError {
                            message: "expected `let` or `use` immediately after `static`"
                                .to_string(),
                            span,
                        });
                        self.builder.start_node_at(
                            cp,
                            FSharpLang::kind_to_raw(SyntaxKind::MEMBER_LET_BINDINGS),
                        );
                        self.builder.finish_node();
                        // No `= <expr>` RHS block was parsed; take the no-RHS-block
                        // terminator so the offside `OBLOCKSEP` before the trailing
                        // `let` is consumed and the loop re-classifies it.
                        false
                    }
                }
                Some(ObjectModelItem::Do) => {
                    // A class-body `do <expr>` binding (9.8d). The cursor is at
                    // the `Virtual::Do`; the reused statement-level
                    // `parse_do_expr` emits the `DO_EXPR` (keyword + offside body)
                    // and self-consumes the body's `OBLOCKEND` and trailing
                    // `ODECLEND`. So no RHS-block terminator is taken: the only
                    // pending virtual left is the body-close `OBLOCKEND` (last
                    // item) or the next item's `OBLOCKSEP`, both handled by the
                    // no-RHS-block terminator below.
                    self.parse_member_do(None);
                    false
                }
                Some(ObjectModelItem::StaticDo) => {
                    // A `static do <expr>` binding (9.8d) — the same `MEMBER_DO`
                    // with a leading `STATIC_TOK`. The cursor is at the raw
                    // `static`; bump it under a checkpoint so the `MEMBER_DO`
                    // reopens *at* the checkpoint, nesting the `STATIC_TOK` inside.
                    let cp = self.builder.checkpoint();
                    self.bump_into(SyntaxKind::STATIC_TOK);
                    // The `do` must *immediately* follow `static` — the LexFilter
                    // relabels the adjacent raw `Token::Do` to `Virtual::Do` only
                    // when they are adjacent. A layout break (`static`⏎`do`, which
                    // leaves an `OBLOCKSEP` before the `Virtual::Do`) is FCS's
                    // "Incomplete structured construct" error; the raw classifier
                    // (virtual-blind) cannot see the separator, so guard here:
                    // only parse the do form when the cursor is at `Virtual::Do`;
                    // else record an error and recover with a bare `STATIC_TOK`-
                    // only `MEMBER_DO`, leaving the trailing `do` to re-classify as
                    // a plain class-body `do` next iteration. (Mirrors the
                    // `static`⏎`let`/`member`/`val` recovery; without it
                    // `parse_do_expr`'s do-at-cursor invariant would misbuild a
                    // nested `DO_EXPR`.)
                    if matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Virtual(Virtual::Do)), _))
                    ) {
                        self.parse_member_do(Some(cp));
                    } else {
                        let span = self
                            .next_non_trivia_raw_at_pos_with_span()
                            .map(|(_, s)| s)
                            .unwrap_or_else(|| self.source.len()..self.source.len());
                        self.errors.push(ParseError {
                            message: "expected `do` immediately after `static`".to_string(),
                            span,
                        });
                        self.builder
                            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEMBER_DO));
                        self.builder.finish_node();
                    }
                    false
                }
                Some(ObjectModelItem::ValField) => {
                    // A `val` field (9.9b) — no `= <expr>` RHS. Threads `member_cp`
                    // so a leading `[<DefaultValue>] val …` attaches (10.7i).
                    self.parse_val_field_at(member_cp);
                    false
                }
                Some(ObjectModelItem::AutoProperty) => {
                    // A `with get[, set]` auto-property consumes its own RHS-close
                    // `OBLOCKEND` (the get/set clause sits after it, inside the
                    // node), so the terminator must not also claim it; a plain
                    // `member val X = e` leaves the RHS-close like a member.
                    // Threads `member_cp` so a leading `[<A>] member val …`
                    // attaches (10.7h).
                    !self.parse_auto_property_at(member_cp)
                }
                Some(ObjectModelItem::NewCtor) => {
                    // An explicit constructor `new(args) = …` (9.10b) — a member
                    // with an offside `= <expr>` RHS, like a method. Threads
                    // `member_cp` so a leading `[<A>] new(…)` attaches (10.7f).
                    // Unlike a regular member, the *unparenthesised* single-arg
                    // form (`new a = …`) opens no RHS block, so use the flag
                    // `parse_new_ctor_at` returns rather than assuming one.
                    self.parse_new_ctor_at(member_cp)
                }
                Some(ObjectModelItem::AbstractSlot) => {
                    // An abstract slot `abstract [member] M : T` (9.10c) — a value
                    // *signature*, no `= <expr>` RHS (like a `val` field). Threads
                    // `member_cp` so a leading `[<A>] abstract …` attaches (10.7g).
                    self.parse_abstract_slot_at(member_cp);
                    false
                }
                Some(ObjectModelItem::Inherit) => {
                    // A base-class clause `inherit Base[(args)] [as base]` (9.11a)
                    // — an `atomType` + optional ctor-args expr, no `= <expr>` RHS
                    // (like a `val` field / abstract slot).
                    self.parse_inherit_member();
                    false
                }
                Some(ObjectModelItem::Interface) => {
                    // An interface implementation `interface I [with member …]`
                    // (9.11b). The `with` block (if any) self-drains its close
                    // virtuals via the shared with-augment loop, so the interface
                    // arrives at the terminator with only the outer body's
                    // separator pending — the no-RHS-block terminator (like a
                    // `val` field).
                    self.parse_interface_member(false);
                    false
                }
                None => break,
            };
            self.consume_object_model_item_terminator(has_rhs_block);
        }
    }

    /// Consume an object-model item's trailing layout virtuals, leaving the
    /// cursor on the next item (so the loop's `classify` continues) or on the
    /// type body's closing `OBLOCKEND` (left for the caller). Shapes
    /// ground-truthed against the filtered stream:
    /// * a member/`let` (`has_rhs_block`) leaves its RHS-close `OBLOCKEND`, then
    ///   either `ODECLEND·OBLOCKSEP` (offside next item), a same-line next item,
    ///   or the body-close `OBLOCKEND` (last item) — we consume the RHS-close and,
    ///   if present, the `ODECLEND` and a single `opt_seps` group;
    /// * a `val` field has no RHS block and leaves only an `OBLOCKSEP` (offside
    ///   next item) or the body-close `OBLOCKEND` (last item);
    /// * a `with get[, set]` auto-property (9.9c) consumes its own RHS-close
    ///   `OBLOCKEND` and the with-clause's `OEND` *inside the node* (the get/set
    ///   tokens follow the RHS-close), so it arrives here with `has_rhs_block =
    ///   false` but may still leave an `ODECLEND` (offside next item) — hence the
    ///   `ODECLEND` is consumed unconditionally below, not only after a RHS-close.
    ///
    /// In every case the body-close `OBLOCKEND` (a lone `OBLOCKEND` *not*
    /// trailed by an `ODECLEND` after the RHS-close has been consumed) is left at
    /// the cursor: the next `classify` returns `None` and the loop stops.
    fn consume_object_model_item_terminator(&mut self, has_rhs_block: bool) {
        if has_rhs_block
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
            )
        {
            // The item's RHS-close `OBLOCKEND`. (For the last item this is the
            // *first* of two `OBLOCKEND`s; the second — the body close — is not
            // an `ODECLEND`, so we leave it below.)
            self.bump_into(SyntaxKind::ERROR);
        }
        // Offside continuation: the `ODECLEND` (item decl end). Follows the
        // RHS-close `OBLOCKEND` (member/`let`/plain auto-prop) or the get/set
        // with-clause's `OEND` (already consumed in the node). A `val` field
        // never emits one. The body-close `OBLOCKEND` is *not* an `ODECLEND`, so
        // a last item leaves it for the caller.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::DeclEnd)), _))
        ) {
            // A decl-flat class-local `let x = e in⏎ <member>`: the swallowed
            // `in` sits behind this `ODECLEND`. Claim it as a clean `IN_TOK` so
            // it does not strand as an "unsupported token In" ERROR. A
            // class-local let-in is always decl-flat — the following item is a
            // member/binding, never a `let … in` body (FCS records
            // `InKeyword = None`, mirroring the module decl-flat form).
            self.claim_swallowed_in();
            self.bump_into(SyntaxKind::ERROR);
        }
        // The inter-item separators — FCS's `opt_seps`, which is a *single*
        // `seps` group: one `;` (`Token::Semi`, a real `SEMI_TOK`) and/or one
        // adjacent offside `OBLOCKSEP` (a zero-width `ERROR`). `type T = val x :
        // int; val y : string` separates two `val` fields with one `;`. (For a
        // member/`let`, a `;` after the RHS is absorbed by the RHS seq-block; a
        // `val` field has no RHS, so its `;` reaches here.)
        let at_close = |p: &Self| {
            matches!(
                p.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _)) | None
            )
        };
        if self.consume_one_seps_group(at_close) {
            // A *repeated* separator (`val x : int; ; val y : int`) is an FCS
            // parse error ("Unexpected symbol ';'") that still recovers both
            // fields. Record one error for the extra group, then drain it so the
            // next item is reached and the member list keeps the same shape.
            let mut reported = false;
            loop {
                let extra = match self.peek() {
                    Some((Ok(FilteredToken::Raw(Token::Semi)), span)) => {
                        Some((span.clone(), SyntaxKind::SEMI_TOK))
                    }
                    Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), span)) => {
                        Some((span.clone(), SyntaxKind::ERROR))
                    }
                    _ => None,
                };
                let Some((span, kind)) = extra else { break };
                if !reported {
                    self.errors.push(ParseError {
                        message: "unexpected separator in member definition".to_string(),
                        span,
                    });
                    reported = true;
                }
                self.bump_into(kind);
            }
        }
    }

    /// Parse a class-body `do <expr>` binding (phase 9.8d) into a
    /// [`SyntaxKind::MEMBER_DO`] node — FCS's `SynMemberDefn.LetBindings(
    /// [SynBinding(kind = Do, …)], isStatic, isRecursive = false, …)` (the
    /// `do`-binding `classDefnBindings` arm). FCS gives the binding a synthetic
    /// `SynPat.Const(Unit)` head and homes the body in `SynBinding.expr` with a
    /// `Do` / `StaticDo` leading keyword; we keep the surface `MEMBER_DO` shape
    /// `[STATIC_TOK?, DO_EXPR]` and the normaliser maps it to the same
    /// `LetBindings` projection.
    ///
    /// The `do` body reuses the statement-level [`Self::parse_do_expr`]: the
    /// cursor is at the `Virtual::Do` (the caller has already bumped any leading
    /// `static`), and `parse_do_expr` emits the `DO_EXPR` (keyword + offside
    /// block) and self-consumes the body's `OBLOCKEND` and trailing `ODECLEND`.
    ///
    /// `outer_cp = Some(checkpoint)` (the `static do` form) reopens the node *at*
    /// the checkpoint so a leading `STATIC_TOK` the caller bumped becomes the
    /// `MEMBER_DO`'s first child; `None` is the plain `do`.
    fn parse_member_do(&mut self, outer_cp: Option<rowan::Checkpoint>) {
        match outer_cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEMBER_DO)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEMBER_DO)),
        }
        self.parse_do_expr();
        self.builder.finish_node(); // MEMBER_DO
    }

    /// Parse a `val` field (`[static] val [mutable] x : T`, phase 9.9b) into a
    /// [`SyntaxKind::VAL_FIELD`] node — `SynMemberDefn.ValField(SynField, _)`.
    /// Shape `[STATIC_TOK?, VAL_TOK, MUTABLE_TOK?, IDENT_TOK, COLON_TOK, <typ>]`.
    /// Unlike a member it has no `= <expr>` RHS (no offside block). FCS's
    /// `valDefnDecl` (`pars.fsy:2168`). Caller has verified the cursor is at a
    /// `val` / `static val` (via [`Self::classify_object_model_item`]).
    ///
    /// With `outer_cp = Some(checkpoint)` the caller has already emitted leading
    /// `ATTRIBUTE_LIST`s (phase 10.7i) after the checkpoint; the `VAL_FIELD` is
    /// opened there so the attributes become its leading children (FCS homes a
    /// `val`-field attribute in `SynField.attributes`, field 0 — the same home as
    /// a record field). The first `bump_into` (`static` / `val`) drains any
    /// leading trivia itself, so no cursor realignment is needed. `None` is the
    /// plain (unattributed) form.
    pub(super) fn parse_val_field_at(&mut self, outer_cp: Option<rowan::Checkpoint>) {
        match outer_cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::VAL_FIELD)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::VAL_FIELD)),
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Static)), _))
        ) {
            self.bump_into(SyntaxKind::STATIC_TOK);
        }
        self.bump_into(SyntaxKind::VAL_TOK);
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Mutable)), _))
        ) {
            self.bump_into(SyntaxKind::MUTABLE_TOK);
        }
        // Optional accessibility modifier — FCS's `valDefnDecl` is
        // `VAL opt_mutable opt_access ident` (so `val mutable internal x`, with
        // access *after* `mutable`; the reverse `val internal mutable x` is an
        // FCS error). Consumed as `ACCESS_TOK` and elided by the normaliser
        // (`SynField.accessibility`, field 6).
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
        // The field name — gate on the *filtered* token so a pending layout
        // close (`OBLOCKEND`) is not seen through: a raw lookahead would spot a
        // later col-0 identifier across the body boundary (`type T =\n  val\nx`)
        // and `bump_into` would consume the zero-width virtual close as the name,
        // losing the type-body close. The filtered cursor stops at the virtual.
        if matches!(
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
                message: "expected a field name after `val`".to_string(),
                span,
            });
        }
        // `: <typ>` — mandatory for a `val` field (FCS errors otherwise).
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
            self.bump_into(SyntaxKind::COLON_TOK);
            self.parse_type();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `:` and a type in a `val` field".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // VAL_FIELD
    }

    /// Parse an auto-property (phase 9.9c) into a [`SyntaxKind::AUTO_PROPERTY`]
    /// node — `SynMemberDefn.AutoProperty`. Shape `[STATIC_TOK?, MEMBER_TOK,
    /// VAL_TOK, ACCESS_TOK?, IDENT_TOK, (COLON_TOK <typ>)?, EQUALS_TOK, <expr>,
    /// (WITH_TOK GET_TOK (COMMA_TOK SET_TOK)?)?]`. FCS's `autoPropsDefnDecl`
    /// (`pars.fsy:2194`); caller has classified `[static] member val` via
    /// [`Self::classify_object_model_item`].
    ///
    /// Returns `true` iff a `with get[, set]` clause was present. That clause
    /// sits *after* the RHS-close `OBLOCKEND` in the filtered stream (`… <expr>
    /// OBLOCKEND OWITH get , set OEND …`), so when present this consumes the
    /// RHS-close (as a child of the node, preserving source order) and the
    /// clause's `OEND`; the caller then passes `has_rhs_block = false` to the
    /// terminator. A plain `member val X = e` leaves its RHS-close like a member
    /// (returns `false`).
    ///
    /// With `outer_cp = Some(checkpoint)` the caller has already emitted leading
    /// `ATTRIBUTE_LIST`s (phase 10.7h) after the checkpoint; the `AUTO_PROPERTY`
    /// is opened there so the attributes become its leading children (FCS homes an
    /// auto-property attribute in `SynMemberDefn.AutoProperty.attributes`, field 0).
    /// The first `bump_into` (the `[static] member` / `override` / `default`
    /// leading keyword) drains any leading trivia itself, so no cursor realignment
    /// is needed. `None` is the plain (unattributed) form.
    fn parse_auto_property_at(&mut self, outer_cp: Option<rowan::Checkpoint>) -> bool {
        match outer_cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::AUTO_PROPERTY)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::AUTO_PROPERTY)),
        }
        // The leading member flags before `val`: `[static] member` (9.9c), or
        // `override`/`default` (9.10a — `override val`/`default val`, FCS leading
        // keyword `OverrideVal`/`DefaultVal`, `pars.fsy:2099`). The classifier
        // verified a `val` follows. The leading keyword is elided by the
        // auto-property normaliser, so only the token shape need be claimed.
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Override)), _)) => {
                self.bump_into(SyntaxKind::OVERRIDE_TOK);
            }
            Some((Ok(FilteredToken::Raw(Token::Default)), _)) => {
                self.bump_into(SyntaxKind::DEFAULT_TOK);
            }
            _ => {
                // Optional `static` (a `static member val`); classifier verified
                // `member` (then `val`) follows.
                if matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Raw(Token::Static)), _))
                ) {
                    self.bump_into(SyntaxKind::STATIC_TOK);
                }
                self.bump_into(SyntaxKind::MEMBER_TOK);
            }
        }
        self.bump_into(SyntaxKind::VAL_TOK);
        // Optional accessibility modifier — FCS's `autoPropsDefnDecl` `opt_access`
        // sits after `val` (`member val private X`). Elided by the normaliser.
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
        // The property name — gate on the *filtered* token so a pending layout
        // close is not seen through (cf. `parse_val_field_at`).
        if matches!(
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
                message: "expected a property name after `member val`".to_string(),
                span,
            });
        }
        // Optional `: <typ>` annotation (`member val X : int = …`).
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
            self.bump_into(SyntaxKind::COLON_TOK);
            self.parse_type();
        }
        // `= <expr>` — the initialiser. Reuses the binding RHS parser, which
        // leaves the RHS-close `OBLOCKEND` at the cursor for the terminator.
        self.parse_let_equals_rhs(false);
        // Optional `with get[, set]`. It follows the RHS-close `OBLOCKEND`, so
        // peek *past* that close for the `OWITH`.
        let has_get_set = matches!(
            self.next_non_trivia_filtered_after_pos(),
            Some(FilteredToken::Virtual(Virtual::With))
        );
        if has_get_set {
            // Consume the RHS-close `OBLOCKEND` inside the node so the get/set
            // tokens (which follow it) stay in source order under AUTO_PROPERTY,
            // leaving the cursor at the `OWITH`.
            self.eat_zero_width_virtual(Virtual::BlockEnd);
            self.parse_member_sig_get_set_clause();
        }
        self.builder.finish_node(); // AUTO_PROPERTY
        has_get_set
    }

    /// Parse a member-signature `with get[, set]` accessor clause — FCS's
    /// `classMemberSpfnGetSet` → `classMemberSpfnGetSetElements`
    /// (`pars.fsy:1051`/`:1071`), the shared production behind both the
    /// auto-property (`member val P = 0 with get, set`, `pars.fsy:2195`) and the
    /// abstract slot (`abstract P : int with get, set`, `pars.fsy:2060`). The
    /// caller has positioned the cursor at the `OWITH` (`Virtual::With`).
    ///
    /// Emits `WITH_TOK`, then one or two comma-separated accessors — each an
    /// optional accessor-specific visibility (`get, private set`; FCS's
    /// `opt_access`, elided as `ACCESS_TOK`) then the `get`/`set` contextual
    /// identifier (`GET_TOK`/`SET_TOK`), in either order (`get, set` /
    /// `set, get`) — and the clause-closing `OEND` (`Virtual::End`, zero-width).
    /// Unlike a member *definition*'s get/set (`with get() = …`,
    /// [`Self::parse_get_set_clause`]) a signature accessor carries no body.
    ///
    /// All of these tokens are elided by the differential normaliser: FCS homes
    /// them in the slot's `SynMemberKind` (`PropertyGet`/`PropertySet`/
    /// `PropertyGetSet`) and trivia, both dropped, so the projection is the
    /// plain name + type regardless of the accessor clause.
    pub(super) fn parse_member_sig_get_set_clause(&mut self) {
        // `with` — `Virtual::With` (`OWITH`) is LexFilter's relabel of a raw
        // `Token::With` at the same span (the `FUN_TOK`/`MATCH` pattern).
        if let Some((Ok(FilteredToken::Virtual(Virtual::With)), with_span)) = self.peek().cloned() {
            self.drain_raw_up_to(with_span.start);
            self.emit_text(SyntaxKind::WITH_TOK, with_span);
            self.raw_pos += 1;
            self.pos += 1;
        }
        // One or two comma-separated accessors; `propKind` is recovered from
        // which appear.
        loop {
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
            // `get`/`set` may be plain or backticked (`` ``get`` ``); FCS's
            // `nameop` dequotes both, so match on the *dequoted* text. The
            // `&'src str` is free of the `peek` borrow (it points into the
            // source), so the `bump_into` in the arms is fine.
            let accessor = self.peek().and_then(|(res, _)| match res {
                Ok(FilteredToken::Raw(t)) => ident_token_text(t),
                _ => None,
            });
            match accessor {
                Some("get") => self.bump_into(SyntaxKind::GET_TOK),
                Some("set") => self.bump_into(SyntaxKind::SET_TOK),
                _ => {}
            }
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Comma)), _))) {
                self.bump_into(SyntaxKind::COMMA_TOK);
                continue;
            }
            break;
        }
        // The with-clause's `OEND` (zero-width close).
        self.eat_zero_width_virtual(Virtual::End);
    }

    /// Parse one `member …` definition into a [`SyntaxKind::MEMBER_DEFN`]
    /// (`SynMemberDefn.Member(SynBinding, _)`, phases 9.7/9.9a) — or, when the
    /// head is followed by a `with` get/set clause, a
    /// [`SyntaxKind::GET_SET_MEMBER`] (`SynMemberDefn.GetSetMember`, phase 9.14).
    ///
    /// A regular member is `[STATIC_TOK?, MEMBER_TOK, BINDING]`: an optional
    /// `static` keyword (a `static member`, 9.9a), the `member` leading keyword
    /// (or `override`/`default`, 9.10a), then a member `SynBinding` whose head is
    /// a member pattern — dotted (`this.M`) or bare (`M`) — via
    /// [`Self::parse_member_head_pat`], its RHS reusing
    /// [`Self::parse_let_equals_rhs`].
    ///
    /// The two forms are not distinguishable until the head is parsed (both start
    /// `member this.P …`), so parse the leading keyword + head under a checkpoint,
    /// then branch on the next token: a `Virtual::With` (OWITH) opens the get/set
    /// clause (retro-wrap as `GET_SET_MEMBER`), anything else is the regular
    /// `= <expr>` RHS (retro-wrap leading-kw under `MEMBER_DEFN`, head under
    /// `BINDING`). Returns `true` iff a get/set member was parsed — it consumes
    /// its own close (the clause's `OEND`), so the caller takes the no-RHS-block
    /// terminator. Caller has verified [`Self::peek_is_object_model_start`].
    ///
    /// With `outer_cp = Some(checkpoint)` the caller has already emitted one or
    /// more leading `ATTRIBUTE_LIST`s (phase 10.7f) after the checkpoint; the
    /// `MEMBER_DEFN`/`GET_SET_MEMBER` is opened at that checkpoint so the attribute
    /// lists become its leading children (FCS homes a member attribute in
    /// `SynBinding.attributes`; for a get/set property it duplicates the attribute
    /// onto both accessor bindings). `None` is the plain (unattributed) form.
    /// Mirrors [`Self::parse_let_decl_at`].
    fn parse_member_defn_at(&mut self, outer_cp: Option<rowan::Checkpoint>) -> bool {
        let cp = outer_cp.unwrap_or_else(|| self.builder.checkpoint());
        // Leading keyword: `[static] member` (9.7/9.9a), or `override`/`default`
        // (9.10a) — the latter introduce a member with no `member` keyword, the
        // binding's `SynLeadingKeyword` distinguishing them (FCS `memberFlags`,
        // `pars.fsy:1580`). The caller's `classify_object_model_item` has verified
        // which keyword leads.
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::Override)), _)) => {
                self.bump_into(SyntaxKind::OVERRIDE_TOK);
            }
            Some((Ok(FilteredToken::Raw(Token::Default)), _)) => {
                self.bump_into(SyntaxKind::DEFAULT_TOK);
            }
            _ => {
                // Optional `static` (a `static member`); the classifier verified a
                // `member` follows it.
                if matches!(
                    self.peek(),
                    Some((Ok(FilteredToken::Raw(Token::Static)), _))
                ) {
                    self.bump_into(SyntaxKind::STATIC_TOK);
                }
                self.bump_into(SyntaxKind::MEMBER_TOK);
            }
        }
        let after_kw = self.builder.checkpoint();
        // Optional `inline` — FCS's `memberCore` (`pars.fsy:1901`) is
        // `opt_inline bindingPattern …`, so the modifier sits between the member
        // flags consumed above and the head pattern (`member inline _.Delay () =
        // …`). LexFilter passes `Token::Inline` through unrewritten. Consumed
        // here, after the `after_kw` checkpoint, so for a regular member it lands
        // inside the `BINDING` opened at `after_kw` below — where
        // `Binding::is_inline` reads it back as FCS's `SynBinding.isInline`. For
        // the get/set property form it becomes a leading bare child of the
        // `GET_SET_MEMBER` (FCS records the flag on both accessor bindings).
        //
        // Unlike a `let` binding (`opt_inline opt_mutable`) there is no
        // `opt_mutable` here: a member cannot be `mutable`, so a stray `mutable`
        // is deliberately left for `parse_member_head_pat` to reject — matching
        // FCS, which has no member production accepting it.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Inline)), _))
        ) {
            self.bump_into(SyntaxKind::INLINE_TOK);
        }
        // A name-position accessibility modifier — FCS's `classDefnMember`
        // `[static] member opt_inline opt_access nameop …` (`pars.fsy:1901`): the
        // `private`/`internal`/`public` sits *after* the member keywords and the
        // `inline` (FCS rejects `member private inline …`) and *before* the name,
        // landing in the head pattern's `accessibility` field. Consumed here as an
        // `ACCESS_TOK` (after the `after_kw` checkpoint, so for a regular member it
        // lands inside the `BINDING`; for the get/set form inside the
        // `GET_SET_MEMBER`) — kept out of ERROR so the parse is lossless; the
        // normaliser elides accessibility on both sides. Mirrors the signature-side
        // `parse_member_sig`, which consumes the same `opt_access` before the name.
        let access_consumed = if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::Internal | Token::Private | Token::Public
                )),
                _,
            ))
        ) {
            self.bump_into(SyntaxKind::ACCESS_TOK);
            true
        } else {
            false
        };
        let paren_value_head = self.parse_member_head_pat(access_consumed);
        // A `with` after the head (OWITH, the `WithAsLet` context) is the explicit
        // get/set property form `member this.P with get() = … [and set …]` (9.14)
        // — distinct from a regular member's `=`. The head `LONG_IDENT_PAT` stays
        // a bare child of the `GET_SET_MEMBER` (no `BINDING` wrapper). A
        // paren-pattern value head (`member (y)`) is *not* a property name, so a
        // `with` after it is an FCS parse error, not a get/set clause: decline here
        // so the `with` falls into `parse_let_equals_rhs`'s "expected `=`" recovery.
        if !paren_value_head
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::With)), _))
            )
        {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::GET_SET_MEMBER));
            self.parse_get_set_clause();
            self.builder.finish_node(); // GET_SET_MEMBER
            true
        } else {
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEMBER_DEFN));
            self.builder
                .start_node_at(after_kw, FSharpLang::kind_to_raw(SyntaxKind::BINDING));
            // A `: T` return-type annotation on the member head — FCS's
            // `memberCore` uses the same `opt_topReturnTypeWithTypeConstraints`
            // production as `localBinding`, projecting identically (the RHS is
            // wrapped in `SynExpr.Typed` by the shared `normalise_binding`).
            // Consumed *after* the `with` check above so the type-annotated
            // get/set head `member P : T with get` — which FCS rejects ("Type
            // annotations on property getters and setters must be given after
            // the accessor") — stays an error here (the `with` falls into
            // `parse_let_equals_rhs`'s "expected `=`" recovery) rather than
            // parsing as a divergent `GET_SET_MEMBER`.
            self.parse_binding_return_info();
            self.parse_let_equals_rhs(false);
            self.builder.finish_node(); // BINDING
            self.builder.finish_node(); // MEMBER_DEFN
            false
        }
    }

    /// Parse a get/set property's `with get() = … [and set v = …]` clause (phase
    /// 9.14), after the property head has been emitted: the `with` (OWITH), one or
    /// two accessors separated by `and`, then the clause's `OEND` (consumed here).
    /// Each accessor is a [`SyntaxKind::GET_SET_ACCESSOR`]. Accessors may appear in
    /// either order (`get … and set …` or `set … and get …`); the `get`/`set`
    /// contextual identifier on each drives its slot. Both inline and offside
    /// accessor bodies are handled (the per-accessor block-close drain below); the
    /// indexer setter `set i v` (FCS bundles its args as a `Tuple`) is deferred.
    fn parse_get_set_clause(&mut self) {
        // `with` — `Virtual::With` (OWITH) is LexFilter's relabel of a raw
        // `Token::With` at the same span (the `parse_auto_property_at` idiom).
        if let Some((Ok(FilteredToken::Virtual(Virtual::With)), with_span)) = self.peek().cloned() {
            self.drain_raw_up_to(with_span.start);
            self.emit_text(SyntaxKind::WITH_TOK, with_span);
            self.raw_pos += 1;
            self.pos += 1;
        }
        // Accessors. Each may carry leading attributes / accessibility / `inline`
        // (FCS's `opt_attributes opt_access opt_inline` before the `get`/`set`),
        // so the loop guard looks *past* those prefixes for the `get`/`set`
        // contextual identifier — otherwise a `… and private set …` would exit the
        // clause early and strand the setter.
        loop {
            if !self.peek_is_get_set_accessor() {
                break;
            }
            self.parse_get_set_accessor();
            // Drain the accessor body's RHS-block close(s). A non-atomic body
            // (`get() = if …`, or an offside `get() =⏎  e`) opens an `OBLOCKBEGIN`;
            // its `OBLOCKEND`(s) are emitted *before* the clause's `OEND` (inner
            // blocks close before the outer `WithAsLet`), but may be deferred past
            // the next accessor when the `and` sits on the body's line. Draining a
            // run of `BlockEnd`s here keeps the cursor at the accessor boundary
            // regardless of layout; it never reaches the enclosing type body's
            // close, which sits *after* the `OEND`. An atomic body opens no block,
            // so this is a no-op.
            while matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockEnd)), _))
            ) {
                self.bump_into(SyntaxKind::ERROR);
            }
            if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::And)), _))) {
                self.bump_into(SyntaxKind::AND_TOK);
                continue;
            }
            break;
        }
        // The clause's `OEND` (zero-width close), consumed inside the node like
        // the auto-property's get/set clause.
        self.eat_zero_width_virtual(Virtual::End);
    }

    /// `true` iff a get/set accessor begins at the cursor — a `get`/`set`
    /// contextual identifier, possibly behind a run of leading prefixes
    /// (attribute lists `[<…>]`, accessibility `private`/`internal`/`public`, and
    /// `inline`; FCS's `opt_attributes opt_access opt_inline`). Scanned on the raw
    /// stream so the clause loop can decide whether another accessor follows an
    /// `and` without committing to consume the prefixes.
    fn peek_is_get_set_accessor(&self) -> bool {
        let mut sig = self.significant_raw_from_cursor();
        let mut tok = sig.next();
        loop {
            match tok {
                // Skip an attribute list `[< … >]` (to its closing `>]`).
                Some(Token::LBrackLess) => {
                    loop {
                        match sig.next() {
                            Some(Token::GreaterRBrack) => break,
                            None => return false,
                            _ => {}
                        }
                    }
                    tok = sig.next();
                }
                Some(Token::Private | Token::Internal | Token::Public | Token::Inline) => {
                    tok = sig.next();
                }
                _ => break,
            }
        }
        matches!(tok.and_then(ident_token_text), Some("get") | Some("set"))
    }

    /// Parse one get/set accessor `[<attrs>] [access] [inline] get[(args)] =
    /// <expr>` / `… set <args> = <expr>` (phase 9.14) into a
    /// [`SyntaxKind::GET_SET_ACCESSOR`] node. Mirrors FCS's per-accessor
    /// `SynBinding`: the optional prefixes (attributes / accessibility / `inline`,
    /// all elided), the `get`/`set` keyword (the binding's `extraId`), the accessor
    /// argument patterns (swept like a member head's curried args), and the
    /// `= <expr>` body. Caller has verified [`Self::peek_is_get_set_accessor`].
    fn parse_get_set_accessor(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::GET_SET_ACCESSOR));
        // Optional leading prefixes, in any order (FCS's `opt_attributes
        // opt_access opt_inline`) — all elided by the normaliser.
        loop {
            match self.peek() {
                Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _)) => {
                    self.parse_attribute_lists();
                }
                Some((
                    Ok(FilteredToken::Raw(Token::Private | Token::Internal | Token::Public)),
                    _,
                )) => {
                    self.bump_into(SyntaxKind::ACCESS_TOK);
                }
                Some((Ok(FilteredToken::Raw(Token::Inline)), _)) => {
                    self.bump_into(SyntaxKind::INLINE_TOK);
                }
                _ => break,
            }
        }
        let (accessor, get_span) = self
            .peek()
            .map(|(res, span)| {
                let text = match res {
                    Ok(FilteredToken::Raw(t)) => ident_token_text(t),
                    _ => None,
                };
                (text, span.clone())
            })
            .unwrap_or((None, self.source.len()..self.source.len()));
        let is_getter = !matches!(accessor, Some("set"));
        match accessor {
            Some("set") => self.bump_into(SyntaxKind::SET_TOK),
            // `get` (and the caller-guaranteed fallthrough) → the getter.
            _ => self.bump_into(SyntaxKind::GET_TOK),
        }
        // A getter must be a function — `get()` or `get(index)`. A bare `get`
        // with no argument pattern (`with get = e`, missing the `()`) is FCS's
        // FS0557 parse error ("A getter property is expected to be a function").
        // The arg sweep below would otherwise silently accept it as an arg-less
        // accessor; detect the absence of any argument (the sweep's own start
        // condition) and flag it. Recovery is unchanged — the accessor still
        // parses (lossless) — so only the diagnostic is added. (A setter takes a
        // value parameter, so it is never subject to this check.)
        if is_getter
            && !self
                .next_non_trivia_raw_at_pos()
                .is_some_and(raw_starts_atomic_pat)
            && !self.folded_signed_literal_at_cursor()
        {
            self.errors.push(ParseError {
                message: "A getter property is expected to be a function, e.g. 'get() = ...' \
                          or 'get(index) = ...'"
                    .to_string(),
                span: get_span,
            });
        }
        // Accessor argument patterns (`get()` / `get(i)` / `set v`) — the shared
        // curried-arg sweep, which also handles the adjacent-paren
        // `HighPrecedenceParenApp` virtual.
        self.sweep_curried_arg_pats();
        // An optional `: T` return type on the accessor (`get() : int = …`).
        // FCS models each accessor as a `SynBinding`, so this is that binding's
        // `returnInfo` — the same `BINDING_RETURN_INFO` node as a `let`/member
        // head; the normaliser wraps the accessor body in `SynExpr.Typed` to
        // match (FCS does the same per-accessor). The arg sweep stops at the
        // `:` (not an atomic-pat start), so the type is not mistaken for a param.
        self.parse_binding_return_info();
        // The `= <expr>` body. Unlike `parse_let_equals_rhs`, do **not** drain to
        // the body block's `OBLOCKEND`: when the body opens a block (`= if …`, or
        // an offside body) the `OBLOCKEND` can be deferred past the `and`/`OEND`,
        // so draining here would swallow the following accessor. The block
        // close(s) are instead drained by the clause loop at the accessor
        // boundary (`parse_get_set_clause`).
        match self.peek().cloned() {
            Some((Ok(FilteredToken::Raw(Token::Equals)), _)) => {
                self.bump_into(SyntaxKind::EQUALS_TOK);
            }
            other => {
                let span = other
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected `=` in a get/set accessor".to_string(),
                    span,
                });
                self.builder.finish_node(); // GET_SET_ACCESSOR
                return;
            }
        }
        // Optional offside-block opener (a non-atomic body), consumed as a
        // zero-width ERROR like `parse_let_equals_rhs`.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::BlockBegin)), _))
        ) {
            self.bump_into(SyntaxKind::ERROR);
        }
        self.parse_seq_block_body("expected an expression after `=` in a get/set accessor");
        self.builder.finish_node(); // GET_SET_ACCESSOR
    }

    /// Parse a member binding's head pattern (phase 9.7) into a
    /// [`SyntaxKind::LONG_IDENT_PAT`] — FCS's `SynPat.LongIdent(longDotId,
    /// extraId=None, typars=None, args=Pats[…], _, _)`. The dotted path
    /// `this.M` (the self-identifier and the member name) is parsed by
    /// [`Self::parse_long_ident_path_with`] into the head `LONG_IDENT`; the
    /// self-id may be a plain identifier (`this.M`) or the wildcard `_`
    /// (`member _.M`, which FCS stores as a `LongIdent` whose first `idText` is
    /// `"_"` — hence `allow_underscore_head`). Curried argument patterns
    /// (`member this.Add a b`) are swept up exactly like the function-form
    /// `let` head ([`Self::try_emit_head_binding_pat_element`]).
    ///
    /// A bare `this.M` (no args) yields `args = Pats[]`, matching FCS for a
    /// property-style member. An *adjacent* parenthesised argument
    /// (`member this.M()` / `member this.M(x)`) is handled by the shared
    /// [`Self::sweep_curried_arg_pats`] (it skips the `HighPrecedenceParenApp`
    /// virtual before the `(`). Return-type annotations
    /// (`member this.M : int = …`), `static`/`abstract` flags, and get/set are
    /// later phase-9 slices.
    ///
    /// `access_consumed` reports whether the caller already bumped a name-position
    /// access modifier (`member private …`); it gates the paren-pattern value-head
    /// route, which FCS's `opt_access nameop` grammar does *not* reach (`member
    /// private (y)` is a parse error). Returns `true` iff a paren-pattern value
    /// head was emitted — the caller uses that to reject a following property
    /// `with get,set` clause (`member (y) with get` is likewise an FCS error).
    fn parse_member_head_pat(&mut self, access_consumed: bool) -> bool {
        // A property-style value member whose name is a *lowercase* single
        // identifier with no self-id and no curried arguments is a `SynPat.Named`,
        // not a `SynPat.LongIdent` — FCS routes the member name through the same
        // `mkSynPatMaybeVar` classifier as a `let` value binding (the head of
        // `static member foo [: T] = …`). We mirror that, emitting a bare
        // `NAMED_PAT > [IDENT_TOK]`, matching the `let`-value path in
        // [`Self::try_emit_atomic_pat`]. Every other shape keeps the
        // `LONG_IDENT_PAT`: an uppercase name (a potential literal/constructor),
        // a self-id'd instance member (`this.M`, dotted), curried args (function
        // form), or a get/set property (`with`). The discriminator is the token
        // immediately after the name: a value member's name is followed by the
        // return-type `:` or the `=`, never an argument-pattern start or `with`.
        if self.member_head_is_named_value() {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::NAMED_PAT));
            self.bump_into(SyntaxKind::IDENT_TOK);
            self.builder.finish_node(); // NAMED_PAT
            return false;
        }
        // An active-pattern-named member head — `static member (|Foo|Bar|) (x, y)
        // = …`, `member (|Foo|_|) y = …` (FCS's `opName` active-pattern member
        // name, reached through the same `pathOp → atomicPatternLongIdent`
        // reduction as the `let (|Foo|Bar|)` binding head). Reuse the binding
        // head's active-pattern machinery verbatim: it emits the same
        // `SynPat.Named` (nullary, var-like) / `SynPat.LongIdent` (curried args /
        // typars) as a value binding's, with the `ACTIVE_PAT_NAME` node as the
        // head segment. The dotted self-id form (`member x.(|Foo|_|)`) keeps a
        // plain ident head and is a later slice (like the operator member head).
        if self.try_emit_active_pat_head() {
            return false;
        }
        // An operator-named member head — `static member (+) (a, b) = …`,
        // `member (?<-) (a, b, c) = …` (FCS's `opName`, reached through the same
        // `pathOp → atomicPatternLongIdent` reduction as the `let (+)` binding
        // head). Reuse the binding head's operator machinery verbatim: a member
        // operator is the same `SynPat.LongIdent` (op name + curried args) /
        // `SynPat.Named` (nullary) as a value binding's. The dotted self-id form
        // (`member x.(+)`) keeps a plain ident head and is a later slice — its
        // `(` falls to `parse_long_ident_path_with`'s dot-continuation, which
        // stays an error as before.
        if let Some(is_star) = self.peek_operator_head() {
            let (after, raw_after) = self.operator_head_after(is_star);
            let has_typars = matches!(
                after,
                Some(
                    FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                        | FilteredToken::Raw(Token::Less(_))
                )
            );
            let has_args = Self::op_head_args_follow(after, raw_after);
            self.emit_operator_head(is_star, has_typars, has_args);
            return false;
        }
        // A *dotted self-id* operator / active-pattern member head — `member
        // x.(+) …`, `member _.(|Foo|_|) …` (FCS folds the self-id and the `opName`
        // into one `SynLongIdent`, `["x"; "op_Addition"]` / `["x"; "|Foo|Bar|"]`).
        // The non-dotted forms above never reach here (their head is the `(`); a
        // dotted one has an ident/`_` self-id, so it falls through to this check.
        // `allow_underscore_head`: a member self-id may be `_` (`member _.(+)`),
        // unlike a `let`-pattern head, whose `_.`-rooted form is language-version
        // gated (see [`Self::peek_dotted_opname_pat_head`]).
        if let Some(kind) = self.peek_dotted_opname_pat_head(true) {
            self.open_dotted_opname_pat_head(kind);
            // The same tails as the plain member head: explicit value-typar decls
            // (`member x.(+)<'T> …`), then the curried argument patterns.
            if self.at_pat_typar_decls() {
                self.parse_typar_decls_postfix(true);
            }
            self.sweep_curried_arg_pats();
            self.builder.finish_node(); // LONG_IDENT_PAT
            return false;
        }
        // A parenthesised-*pattern* member head — `static member (y) = 0`,
        // `static member (y: int) = 0`, `member (Some x) = 0` (the `neg133.fs`
        // fixtures). FCS's member binding pattern is a full `headBindingPattern`
        // (`classDefnBindings → defnBindings`, the same production a `let` value
        // binding uses), so a `(` that is *not* an operator / active-pattern /
        // glued-star name — all of which are claimed above and `return` — opens an
        // ordinary paren *pattern* `SynPat.Paren(pat)`, exactly as `let (y) = …`
        // does; the member name is derived from the pattern in a later phase. A
        // paren-pattern head is a *value* head (FCS rejects a curried `member (y)
        // z`), so route it through the shared `let`-head parser
        // ([`Self::parse_head_binding_pat`]), which handles the paren element and
        // any top-level tuple / `as` / `::` tail FCS's `headBindingPattern` admits
        // — rather than the applied `LONG_IDENT_PAT` path below (whose
        // `parse_long_ident_path_with` would reject the leading `(`). Purely
        // additive: every `(`-headed member that is not an operator/active/star
        // name previously fell through to that path and errored.
        //
        // Gated on `!access_consumed`: this value-binding form is FCS's
        // `defnBindings` member, which has no `opt_access` — `member private (y)`
        // is a parse error there. When an access modifier *was* bumped, decline so
        // the head falls through to the `LONG_IDENT_PAT` path, which rejects the
        // leading `(` exactly as before this route existed. Returns `true` so the
        // caller rejects a following property `with get,set` (equally an error on a
        // value binding).
        if !access_consumed
            && matches!(
                self.peek(),
                Some((Ok(FilteredToken::Raw(Token::LParen)), _))
            )
        {
            self.parse_head_binding_pat();
            return true;
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_PAT));
        self.parse_long_ident_path_with("member", true);
        // Explicit value-typar decls after the name (`member M<'a>(x) = …`,
        // `static member F<'T when 'T :> I>() = …`). FCS's `memberCore` carries
        // `opt_explicitValTyparDecls` between the name and the args, stored on
        // `SynPat.LongIdent.typarDecls` — the same `<'a>` the `let` head and an
        // active-pattern head accept. The adjacent form is preceded by
        // LexFilter's `HighPrecedenceTyApp` virtual, the spaced one by a bare
        // `Less`; either at the post-name cursor opens the typar list.
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                    | FilteredToken::Raw(Token::Less(_))),
                _,
            ))
        ) {
            // `permit_empty = true`: a value binding's `explicitValTyparDeclsCore`
            // accepts an empty `< >`.
            self.parse_typar_decls_postfix(true);
        }
        // Curried argument patterns — the shared function-form sweep (which also
        // handles an adjacent paren arg `member this.M(x)` via the
        // `HighPrecedenceParenApp` virtual). The raw lookahead stops at `=` (not
        // an atomic-pat start) for an arg-less member.
        self.sweep_curried_arg_pats();
        self.builder.finish_node(); // LONG_IDENT_PAT
        false
    }

    /// `true` iff the member head at the cursor is a property-style value member
    /// whose pattern FCS models as `SynPat.Named` (see [`Self::parse_member_head_pat`]):
    /// a single non-uppercase-leading identifier (per [`ident_text_leads_uppercase`],
    /// which also covers backtick-quoted names), with no self-id (no dotted tail,
    /// per [`Self::pat_head_has_dotted_tail`]), immediately followed by the
    /// return-type `:` or the binding `=` (so no curried arguments and no get/set
    /// `with` clause). Mirrors the lowercase-single-ident arm of
    /// [`Self::try_emit_atomic_pat`].
    fn member_head_is_named_value(&self) -> bool {
        let Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), span)) =
            self.peek()
        else {
            return false;
        };
        let text = &self.source[span.clone()];
        if ident_text_leads_uppercase(text) || self.pat_head_has_dotted_tail() {
            return false;
        }
        matches!(
            self.next_non_trivia_filtered_after_pos(),
            Some(FilteredToken::Raw(Token::Equals | Token::Colon))
        )
    }

    /// Parse an explicit constructor `new(args) [as self] = …` (phase 9.10b) into
    /// a [`SyntaxKind::MEMBER_DEFN`] node — FCS's `classDefnMember` NEW arm
    /// (`pars.fsy:2106`) yields `SynMemberDefn.Member(SynBinding …)` whose head is
    /// `SynPat.LongIdent(SynLongIdent(["new"]), …, args=Pats[atomicPattern])`,
    /// leading keyword `New`, `valData` `MemberKind=Constructor`. We mirror that
    /// with the same `MEMBER_DEFN`/`BINDING` shape as a method: the head
    /// `LONG_IDENT_PAT` carries the `new` keyword (a `NEW_TOK`) as its sole
    /// `LONG_IDENT` segment (read back as `"new"`), then the single
    /// `atomicPattern` argument (the general pattern parser, so `()` is a
    /// `Paren(Const Unit)` — *not* the implicit ctor's bare `Const Unit`). An
    /// optional `as <self>` (`optAsSpec`, FCS's `valData.thisIdOpt`) follows,
    /// elided by the normaliser. The RHS reuses [`Self::parse_let_equals_rhs`].
    /// Caller has verified the cursor is at the raw `new` keyword.
    ///
    /// With `outer_cp = Some(checkpoint)` the caller has already emitted leading
    /// `ATTRIBUTE_LIST`s (phase 10.7f) after the checkpoint; the `MEMBER_DEFN` is
    /// opened there so the attributes become its leading children (FCS models
    /// `[<A>] new(…)` as a `Member(SynBinding)`, homing the attribute in
    /// `SynBinding.attributes` — projected from the `MEMBER_DEFN` onto the binding,
    /// like a regular member). `None` is the plain (unattributed) form.
    ///
    /// Returns whether the RHS opened an offside block (see
    /// [`Self::parse_let_equals_rhs`]). A parenthesised ctor (`new(a) = …`) opens
    /// one, like a regular member; the *unparenthesised* single-arg form (`new a
    /// = …`) does not — LexFilter emits no `OBLOCKBEGIN` after its `=`, matching
    /// FCS — so its terminator must not consume a (non-existent) RHS-close
    /// `OBLOCKEND`. The caller threads this into
    /// [`Self::consume_object_model_item_terminator`]'s `has_rhs_block`.
    fn parse_new_ctor_at(&mut self, outer_cp: Option<rowan::Checkpoint>) -> bool {
        match outer_cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::MEMBER_DEFN)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::MEMBER_DEFN)),
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::BINDING));
        // Optional accessibility modifier (`private new(…)` — FCS's `opt_access`
        // before `NEW`; it lands in the head pattern's `accessibility`). Elided by
        // the normaliser, like other accessibility.
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
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT_PAT));
        // The head: the `new` keyword *is* the binding's single path segment.
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
        self.bump_into(SyntaxKind::NEW_TOK);
        self.builder.finish_node(); // LONG_IDENT
        // The constructor argument — a single `atomicPattern` (`(a)` / `()` /
        // `(a, b)`). No `HighPrecedenceParenApp` precedes a `new`'s `(`.
        if !self.try_emit_atomic_pat() {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected constructor arguments after `new`".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // LONG_IDENT_PAT
        // Optional `as <self-id>` (`optAsSpec`) — same shape as the implicit
        // ctor's; elided by the normaliser (FCS stores it in `valData.thisIdOpt`).
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
        let opened_block = self.parse_let_equals_rhs(false);
        self.builder.finish_node(); // BINDING
        self.builder.finish_node(); // MEMBER_DEFN
        opened_block
    }

    /// Parse an abstract slot `abstract [member] Name : <type>` (phase 9.10c)
    /// into a [`SyntaxKind::ABSTRACT_SLOT`] node — FCS's `classDefnMember`
    /// abstract arm (`pars.fsy:2060`) + `abstractMemberFlags` (`:1973`) yield
    /// `SynMemberDefn.AbstractSlot(slotSig: SynValSig, flags, …)`. The slot is a
    /// value *signature* with no `= <expr>` body, so (like a `val` field) it takes
    /// the no-RHS-block terminator. Shape
    /// `[ABSTRACT_TOK, MEMBER_TOK?, VAL_SIG, (WITH_TOK get/set…)?]`, where the
    /// [`SyntaxKind::VAL_SIG`] child (`[IDENT_TOK, COLON_TOK, <type>]`) is the
    /// `SynValSig` carrier shared with phase 10.12, and the optional trailing
    /// `with get[, set]` accessor clause ([`Self::parse_member_sig_get_set_clause`])
    /// marks a *property* slot. Caller has verified the cursor is at the raw
    /// `abstract` keyword.
    ///
    /// The signature type routes through [`Self::parse_type_with_constraints_top`]
    /// (FCS's `topTypeWithTypeConstraints`, `pars.fsy:2060`), so an un-named
    /// function type (`int -> int`, curried, or a bare `int` property), a
    /// trailing `when` clause (`'T -> 'T when 'T : comparison` →
    /// `SynType.WithGlobalConstraints`), and — this being a `topType` —
    /// named/optional parameter signatures (`x: int -> int`, `?x: int` →
    /// `SynType.SignatureParameter`, phase 10.12b) are all covered.
    ///
    /// Only an *identifier* name is reached here (the classifier withholds an
    /// operator/active-pattern `nameop`, `abstract (+) : …`, `abstract (|Foo|_|) : …`).
    /// Operator- and active-pattern-named *abstract slots* remain a gap (this slot
    /// parser accepts only identifiers). The other member name slots — the
    /// member-sig ([`Self::parse_member_sig`]), the member-def head
    /// ([`Self::parse_member_head_pat`]), the `val` sig
    /// ([`Self::parse_val_sig_decl_at`]) and the `let` binding head
    /// ([`Self::try_emit_head_binding_pat_element`]) — all handle these names.
    ///
    /// With `outer_cp = Some(checkpoint)` the caller has already emitted leading
    /// `ATTRIBUTE_LIST`s (phase 10.7g) after the checkpoint; the `ABSTRACT_SLOT` is
    /// opened there so the attributes become its leading children (FCS homes an
    /// abstract-slot attribute in `SynValSig.attributes`). `bump_into(ABSTRACT_TOK)`
    /// drains any leading trivia itself, so no cursor realignment is needed. `None`
    /// is the plain (unattributed) form.
    fn parse_abstract_slot_at(&mut self, outer_cp: Option<rowan::Checkpoint>) {
        match outer_cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::ABSTRACT_SLOT)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::ABSTRACT_SLOT)),
        }
        // Optional leading `static` (`static abstract [member] M` — the F# 7 IWSAM
        // static-abstract interface slot). The classifier routes both the
        // `abstract …` and `static abstract …` forms here; the `STATIC_TOK`
        // distinguishes the `StaticAbstract`/`StaticAbstractMember` leading
        // keywords from `Abstract`/`AbstractMember` (the normaliser reads it via
        // [`crate::syntax::AbstractSlot::is_static`]).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Static)), _))
        ) {
            self.bump_into(SyntaxKind::STATIC_TOK);
        }
        self.bump_into(SyntaxKind::ABSTRACT_TOK);
        // The optional `member` keyword (`abstract member M`, leading keyword
        // `AbstractMember` vs bare `abstract`'s `Abstract`).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Member)), _))
        ) {
            self.bump_into(SyntaxKind::MEMBER_TOK);
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::VAL_SIG));
        // The optional `access`/`inline` modifiers before the name (FCS's
        // `opt_access opt_inline`). `inline` → `SynValSig.isInline`; an `access`
        // modifier is *illegal* on an abstract slot (it inherits the type's
        // visibility) — FCS reports a diagnostic but still builds the slot, so we
        // do the same: record an error, consume the token (elided), and carry on.
        // Both are elided by the normaliser, so their order is immaterial; consume
        // a lenient run of either.
        loop {
            match self.peek() {
                Some((Ok(FilteredToken::Raw(Token::Inline)), _)) => {
                    self.bump_into(SyntaxKind::INLINE_TOK);
                }
                Some((
                    Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
                    span,
                )) => {
                    let span = span.clone();
                    self.errors.push(ParseError {
                        message: "accessibility modifiers are not permitted on an abstract member \
                                  (it has the same visibility as the enclosing type)"
                            .to_string(),
                        span,
                    });
                    self.bump_into(SyntaxKind::ACCESS_TOK);
                }
                _ => break,
            }
        }
        // The slot name (`nameop`). Gate on the *filtered* token so a pending
        // layout close is not seen through (cf. `parse_val_field_at`). An
        // operator (`abstract (+) : …`) or active-pattern (`abstract (|Foo|_|) :
        // …`) name reuses the binding-head machinery, exactly as
        // [`Self::parse_member_sig`] does — it emits the name directly as the
        // `SynValSig` name (the source spelling under `IDENT_TOK` / the folded
        // `ACTIVE_PAT_NAME`; the differential normaliser unwraps FCS's mangled
        // `op_*` + `OriginalNotation` / rebuilds the active-pattern `idText`). No
        // curried args follow a *signature* name — the arg types live in the `:
        // <type>` below — so only the name is consumed (explicit typars, if any,
        // are taken by the postfix-typar parse below). The active-pattern check
        // precedes the operator head: both open with `(`, but only its second
        // token is the `|` that `peek_operator_head` excludes.
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
                message: "expected an abstract member name".to_string(),
                span,
            });
        }
        // Optional postfix `< … >` type parameters (`abstract M<'U> : …`, FCS's
        // `opt_explicitValTyparDecls` → `SynValSig.explicitTypeParams`, elided).
        // The `<` is adjacent (a `HighPrecedenceTyApp` virtual) or spaced (a bare
        // `Less`), exactly like a type header's typars — reuse that parser for the
        // common forms. The value-signature-only extensions (the `, ..` flex list
        // and the empty `<>`, `explicitValTyparDeclsCore`) are not modelled by the
        // type-header parser; they are a clean (lossless) error here, deferred to
        // phase 10.12's proper `SynValTyparDecls` parser (typars are elided by the
        // normaliser regardless, so the *common* forms diff-match unchanged).
        if matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Virtual(Virtual::HighPrecedenceTyApp)
                    | FilteredToken::Raw(Token::Less(_))),
                _,
            ))
        ) {
            // `permit_empty = false`: the empty `<>` value-signature form stays a
            // clean (lossless) error here, deferred to phase 10.12 (see above).
            self.parse_typar_decls_postfix(false);
        }
        // The mandatory `: <type>` signature. FCS's `abstractMemberFlags` arm
        // ends in `COLON topTypeWithTypeConstraints` (`pars.fsy:2060`), so a
        // trailing `when` clause folds the type into `SynType.WithGlobalConstraints`
        // and — being a `topType` — a named/optional parameter
        // (`abstract M : x: int -> int`, `?x: int`) lowers to
        // `SynType.SignatureParameter`. Route through
        // `parse_type_with_constraints_top` (the `topType` wrapper the `.fsi`
        // val/member sigs use, phase 10.12b), not the general `parse_type`.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
            self.bump_into(SyntaxKind::COLON_TOK);
            self.parse_type_with_constraints_top();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `:` and a type in an abstract member signature".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // VAL_SIG
        // Optional `with get[, set]` accessor clause (FCS's `classMemberSpfnGetSet`,
        // `pars.fsy:2060`) — a *property* abstract slot. Unlike the auto-property
        // (whose `= <expr>` RHS opens a block that closes with an `OBLOCKEND`
        // before the `OWITH`), the slot's type opens no block, so the `OWITH`
        // sits directly at the cursor. The clause's tokens become trailing
        // children of `ABSTRACT_SLOT`, after the `VAL_SIG`; they only drive the
        // slot's `SynMemberKind`/trivia in FCS (both elided by the normaliser),
        // and none collide with the `val_sig`/`attributes`/`is_abstract_member`
        // accessors. Gate on the `OWITH` so the plain (method/property) slot is
        // untouched (the helper is also a no-op when none is present, but the
        // guard keeps the common path obvious).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Virtual(Virtual::With)), _))
        ) {
            self.parse_member_sig_get_set_clause();
        }
        self.builder.finish_node(); // ABSTRACT_SLOT
    }

    /// Parse a base-class clause `inherit <atomType> [args] [as base]` (phase
    /// 9.11a) into a [`SyntaxKind::INHERIT_MEMBER`] node — FCS's `inheritsDefn`
    /// (`pars.fsy:2330`). Two `SynMemberDefn` cases share this node, the diff
    /// keyed on whether constructor args follow:
    ///
    /// * `inherit Base()` → `SynMemberDefn.ImplicitInherit(inheritType,
    ///   inheritArgs, inheritAlias, …)` — the args are present
    ///   (`atomicExprAfterType`: `()` → `Const Unit`, `(a, b)` →
    ///   `Paren(Tuple)`);
    /// * `inherit Base` → `SynMemberDefn.Inherit(Some baseType, asIdent, …)` —
    ///   no args.
    ///
    /// The base type is FCS's `atomType` ([`Self::parse_atomic_type`], so
    /// `Base<int>` keeps its `<…>` inside the type and a following `(` is left
    /// for the args). The args reuse the attribute-argument machinery (FCS's
    /// `opt_HIGH_PRECEDENCE_APP atomicExprAfterType`, the same shape as
    /// [`Self::parse_attribute`]): an adjacent `(` carries a
    /// `HighPrecedenceParenApp` marker, consumed zero-width before
    /// [`Self::parse_atomic_expr`]. The optional `as <ident>` (`optBaseSpec` —
    /// FCS normalises the ident to `base` and elides it) is consumed and elided.
    /// There is no `= <expr>` RHS, so (like a `val` field) the caller takes the
    /// no-RHS-block terminator. Caller has verified the cursor is at the raw
    /// `inherit` keyword.
    fn parse_inherit_member(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INHERIT_MEMBER));
        self.bump_into(SyntaxKind::INHERIT_TOK);
        // The base class — FCS's `atomType`. A missing type is FCS's third
        // `inheritsDefn` production (`Inherit(None, …)` recovery): record an
        // error and leave the node typeless (the facade's `base_type()` → None).
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
        // Optional constructor args (`atomicExprAfterType`) — their presence is
        // the `ImplicitInherit`-vs-`Inherit` discriminant. An adjacent `(` is
        // preceded by the `HighPrecedenceParenApp` marker (consumed zero-width);
        // the gate ([`Self::peek_starts_aftertype_arg`], shared with
        // `parse_attribute` / `parse_new_expr`) admits a `(` paren/unit arg but
        // not the `( op )` operator-value (excluded from `atomicExprAfterType`),
        // and otherwise the `atomicExprAfterType` starters — which exclude a
        // bare ident and `as`.
        if self.peek_is_paren_app_marker() {
            self.bump_into(SyntaxKind::ERROR);
        }
        let starts_arg = self.peek_starts_aftertype_arg();
        if starts_arg {
            self.parse_atomic_expr();
        }
        // Optional `as <alias>` (`optBaseSpec`/`baseSpec`) — shared with the
        // object-expression base call (`{ new T(args) as base with … }`).
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::As)), _))) {
            self.parse_base_spec();
        }
        self.builder.finish_node(); // INHERIT_MEMBER
    }

    /// Parse the optional base alias `as <alias>` — FCS's `baseSpec`
    /// (`pars.fsy:2368`), shared by `inherit Base(args) as base` (phase 9.11a,
    /// [`Self::parse_inherit_member`]) and the object-expression base call
    /// `{ new T(args) as base with … }` (`objExprBaseCall`'s trailing
    /// `baseSpec`, [`Parser::parse_obj_or_computation_brace`]). FCS's two
    /// productions both rewrite the alias to `base` and elide it, but differ on
    /// the diagnostic (`parsInheritDeclarationsCannotHaveAsBindings`, FS0564 —
    /// emitted verbatim for *both* the inherit and object-expression sites): the
    /// `AS BASE` form (the `base` *keyword*) *always* errors, while the
    /// `AS ident` form errors **unless** the de-quoted idText is `base` — so a
    /// quoted `` as ``base`` `` is the one *valid* alias form.
    ///
    /// Consume the `as` + alias for lossless coverage, recording the error in
    /// every case except a `base`-text identifier. The caller has verified the
    /// cursor is at the raw `as` ([`Token::As`]); emits `AS_TOK` (+ the alias as
    /// `IDENT_TOK`) into the currently open node.
    pub(super) fn parse_base_spec(&mut self) {
        let as_span = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::As)), s)) => s.clone(),
            _ => return,
        };
        self.bump_into(SyntaxKind::AS_TOK);
        // Classify the alias on the raw stream (it is adjacent to `as`, no
        // intervening virtual). A `base`-text identifier (`base` quoted as
        // `` ``base`` ``) is the valid `AS ident` form; the bare `base`
        // keyword is the always-erroring `AS BASE` form.
        let (alias_present, valid_base_alias) = match self.next_non_trivia_raw_at_pos() {
            Some(t @ (Token::Ident(_) | Token::QuotedIdent(_))) => {
                (true, ident_token_text(t) == Some("base"))
            }
            Some(Token::Base) => (true, false),
            _ => (false, false),
        };
        if alias_present {
            self.bump_into(SyntaxKind::IDENT_TOK);
        }
        if !valid_base_alias {
            self.errors.push(ParseError {
                message: "'inherit' declarations cannot have 'as' bindings (use `base.Member`)"
                    .to_string(),
                span: as_span,
            });
        }
    }

    /// Parse an interface implementation `interface I [with member …]` (phase
    /// 9.11b) into a [`SyntaxKind::INTERFACE_IMPL`] node — FCS's `classDefnMember`
    /// interface arm (`pars.fsy:2044`) yields `SynMemberDefn.Interface(
    /// interfaceType, withKeyword, members: SynMemberDefns option, range)`.
    ///
    /// The `interface` keyword in member position arrives as LexFilter's
    /// `OINTERFACE_MEMBER` ([`Virtual::InterfaceMember`]), a relabel of the raw
    /// `Token::Interface` carrying the keyword's span; emit it losslessly as an
    /// `INTERFACE_TOK` the `OWITH`/`OffsideLet` way (`emit_text` + advance both
    /// cursors — `bump_into` would zero-width it and orphan the keyword text).
    /// The interface type is FCS's `appTypeWithoutNull` ([`Self::parse_app_type`],
    /// one step broader than 9.11a's `atomType`: `I` / `I<int>` / `Foo.IBar`).
    ///
    /// An optional `with member …` block (`opt_interfaceImplDefn`,
    /// `pars.fsy:2305`) is the `members: Some` form; its filtered stream is
    /// byte-identical to the 9.13a/9.15b with-augment (raw `WITH`, then
    /// `OBLOCKBEGIN`, the members, `OBLOCKEND`, `ODECLEND`), so it reuses
    /// [`Self::parse_with_augmentation_members`] — the members nest **inside** the
    /// open `INTERFACE_IMPL` (the interface's own member list), and the helper
    /// drains the with-block's close virtuals, leaving the outer body's separator
    /// for the caller's terminator. No `with` is the bare `members: None` form.
    /// Caller has verified the cursor is at the `OINTERFACE_MEMBER` virtual.
    /// `sig` selects the *signature* form (`SynMemberSig.Interface`, phase 10.14
    /// slice 3b): just `interface <appTypeWithoutNull>`, with **no** member list.
    /// FCS's sig interface grammar has no `with`-block, so a trailing `with` is
    /// left unconsumed (the caller's trailing-body handling skips it) rather than
    /// parsed as impl-style `with member … = …` definitions. The impl form
    /// (`sig = false`, `SynMemberDefn.Interface`) keeps the optional `with`.
    pub(super) fn parse_interface_member(&mut self, sig: bool) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INTERFACE_IMPL));
        // The relabelled `interface` keyword (the `OWITH`/`OffsideLet` pattern):
        // the virtual carries the keyword's span, and the raw `Token::Interface`
        // still sits at `raw_pos` with that span. Drain preceding trivia into the
        // open node (like `bump_into`), emit the keyword text, advance both.
        let kw_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_interface_member invoked without a peeked interface token");
        // Drain any leading trivia up to the keyword first: the type-definition
        // member loop reaches this with `raw_pos` already at the `interface`
        // keyword, but the object-expression interface-only form
        // (`{ new T() interface I … }`) arrives straight off the base call with
        // the inter-token whitespace still pending, so `raw_pos` sits on trivia.
        // Draining first lands `raw_pos` on the keyword, so the backing-token
        // assertion below holds for both callers.
        self.drain_raw_up_to(kw_span.start);
        debug_assert!(
            matches!(
                self.raw_tokens.get(self.raw_pos),
                Some((Ok(TriviaToken::Lexed(Token::Interface)), s)) if *s == kw_span,
            ),
            "interface token must be backed by a raw Token::Interface at raw_pos with matching span"
        );
        self.emit_text(SyntaxKind::INTERFACE_TOK, kw_span);
        self.raw_pos += 1;
        self.pos += 1;

        // The implemented interface type (`appTypeWithoutNull`). A missing type is
        // FCS's `interfaceMember recover` arm: record an error and leave the node
        // typeless (the facade's `interface_type()` → None).
        if self.peek_starts_atomic_type() {
            self.parse_app_type();
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected an interface type after `interface`".to_string(),
                span,
            });
        }

        // Optional `with member …` block (impl form only). The raw `with` (the
        // `WithAsAugment` context) opens the same offside member block as a type
        // augmentation, so emit a `WITH_TOK` and delegate to the shared loop; its
        // members become direct children of this open `INTERFACE_IMPL` node (the
        // interface's own member list). No `with` → the bare `interface I`
        // (`members = None`). In a *signature* (`sig`) there is no member list, so
        // a trailing `with` is left for the caller's trailing-body handling.
        if !sig && matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::With)), _))) {
            self.bump_into(SyntaxKind::WITH_TOK);
            // The members, and any explicit `end` closer (`interface I with
            // <members> end`, FCS's `opt_interfaceImplDefn`), are handled by the
            // shared augment loop — the `end` lands as an inert `END_TOK` child of
            // this open `INTERFACE_IMPL`, matching the offside-closed form. Pass
            // `empty_block_takes_end = false`: FCS's interface-implementation
            // `with`-block requires at least one member, so an *empty*
            // `interface I with end` is a parse error — its `end` is left for the
            // caller's stray-token recovery rather than consumed into a
            // valid-looking empty implementation. (Type/exception augmentations
            // pass `true`, since an empty augmentation *is* FCS-valid.)
            self.parse_with_augmentation_members(false, false);
        }
        self.builder.finish_node(); // INTERFACE_IMPL
    }

    /// Attempt to parse one top-level declaration. Returns `false` if the
    /// current token doesn't begin any known expression form, leaving the
    /// cursor in place for the caller to recover.
    pub(super) fn parse_module_decl(&mut self) -> bool {
        let Some((_, span)) = self.peek().cloned() else {
            return false;
        };
        if !self.peek_is_expr_start() {
            return false;
        }
        // Drain leading trivia (and any LexFilter-swallowed raws) out to the
        // expression's start *before* opening the decl node, so the trivia
        // lands as a sibling of `EXPR_DECL` rather than a child.
        // rust-analyzer-style: a node's text range covers only the tokens it
        // semantically owns. LSP consumers walk ancestors of a token to find
        // enclosing declarations; trivia leaking into the expression would
        // put comments/whitespace under the wrong ancestor.
        self.drain_raw_up_to(span.start);
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::EXPR_DECL));
        self.parse_expr();
        self.builder.finish_node();
        true
    }

    /// `pars.fsy:1396 openDecl` — `OPEN path` →
    /// `SynModuleDecl.Open(SynOpenDeclTarget.ModuleOrNamespace(SynLongIdent,
    /// _), _)`, and `OPEN typeKeyword appTypeWithoutNull` →
    /// `SynOpenDeclTarget.Type(SynType)`. Shape:
    /// `OPEN_DECL > [OPEN_TOK, LONG_IDENT]` for the module/namespace target;
    /// `OPEN_DECL > [OPEN_TOK, TYPE_TOK, <type>]` for `open type T`.
    ///
    /// `open` flows through the filtered stream unchanged (it opens no
    /// LexFilter context). The `type` keyword in `open type T`, by contrast,
    /// is *swallowed* by LexFilter (it pushes a transient `CtxtTypeDefns`
    /// exactly like a bare `type` definition), so it never reaches the
    /// filtered stream — we recover it from the raw stream and claim it as
    /// `TYPE_TOK`, the same way [`Self::parse_let_head_and_bindings`] claims the
    /// raw `let`. Caller must have verified `peek()` is `Token::Open`.
    pub(super) fn parse_open_decl(&mut self) {
        // Keep leading trivia/comments as a sibling of OPEN_DECL, matching
        // the convention in `parse_module_decl` / `parse_let_decl_at`.
        let open_span = self
            .peek()
            .map(|(_, s)| s.clone())
            .expect("parse_open_decl invoked without a peeked Token::Open");
        self.drain_raw_up_to(open_span.start);

        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::OPEN_DECL));
        self.bump_into(SyntaxKind::OPEN_TOK);

        // The open target must begin on `open`'s own logical line. If the
        // next *filtered* token is a layout virtual (`OBLOCKSEP`/`ODECLEND`)
        // or EOF — i.e. `open` stands alone — the open is incomplete: emit an
        // empty `LONG_IDENT` and record the error (FCS's `OPEN recover`,
        // `pars.fsy:1399`) WITHOUT crossing the layout boundary. Skipping this
        // would let the raw-stream `type` lookahead below claim a *following*
        // declaration's swallowed `type` (`open⏎type Foo = int`), since the
        // raw scan steps over the newline that the `OBLOCKSEP` marks. A path
        // on an indented *continuation* line (`open⏎    System`) is still
        // accepted: LexFilter emits no `OBLOCKSEP` there, so the next filtered
        // token is the path ident, not a virtual.
        if !matches!(self.peek(), Some((Ok(FilteredToken::Raw(_)), _))) {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected identifier after `open`".to_string(),
                span,
            });
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::LONG_IDENT));
            self.builder.finish_node();
            self.builder.finish_node(); // OPEN_DECL
            return;
        }

        // `open type T`: LexFilter swallowed the `type` keyword, so the
        // filtered cursor is already at the type's first token. Detect the
        // swallowed `type` on the raw stream and emit it as `TYPE_TOK`, then
        // parse the trailing type. Otherwise the target is a plain dotted
        // path (`open Foo.Bar`).
        let swallowed_type_span = match self.next_non_trivia_raw_at_pos_with_span() {
            Some((Token::Type, span)) => Some(span),
            _ => None,
        };
        if let Some(type_span) = swallowed_type_span {
            self.drain_raw_up_to(type_span.start);
            self.emit_text(SyntaxKind::TYPE_TOK, type_span.clone());
            self.raw_pos += 1;
            // FCS's `openDecl` target is `appTypeWithoutNull`
            // (`pars.fsy:1402`), NOT the full `typ`: top-level `->`, `*`, and
            // `| null` *terminate* the open target rather than extend it
            // (`open type A -> B` opens `A`, leaving `-> B`; `open type int |
            // null` opens `int`). Call the app-type layer directly — the
            // level below the nullable / tuple / arrow layers — reproducing
            // `parse_type`'s starter guard and leading-trivia drain so the
            // whitespace between `type` and the type stays a sibling.
            //
            // Known limitation: a *global-qualified* type target
            // (`open type global.System.Math`) is rejected here with
            // "expected type", because the shared type parser doesn't yet
            // accept `global` as a type-path root — `(x : global.Foo)` fails
            // the same way. That is a pre-existing phase-7 gap (`global` is
            // absent from `raw_starts_atomic_type` and `parse_atomic_type`'s
            // head arm), not an open-decl-specific one, and is deferred to
            // the type parser rather than special-cased here (a local hack
            // would make `global` work in `open type` but not in `(x : T)`).
            // The failure is clean and visible (error + no type child), never
            // corruption.
            if self.peek_starts_type_or_anon_recd() {
                if let Some((_, span)) = self.peek() {
                    let start = span.start;
                    self.drain_raw_up_to(start);
                }
                self.parse_app_type();
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected type".to_string(),
                    span,
                });
            }
        } else {
            self.parse_long_ident_path("open");
        }

        self.builder.finish_node();
    }
}
