//! Structural type-representation productions: record, union, enum, and
//! exception reprs (both the impl-side `exception` defn and its `.fsi`
//! counterpart).

use super::*;

impl<'src> Parser<'src> {
    /// `true` iff the body begins a record repr — a `{`, optionally preceded by
    /// a repr-level access modifier (`type T = private { … }`, FCS's
    /// `opt_access braceFieldDeclList`). An access modifier is only treated as a
    /// record start when a `{` follows it on the raw stream; before a type it is
    /// left for the abbreviation path (where FCS errors on abbreviation
    /// visibility).
    pub(super) fn peek_is_record_repr_start(&self) -> bool {
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::LBrace)), _)) => true,
            Some((
                Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
                span,
            )) => matches!(
                self.next_non_trivia_raw_after(span.end),
                Some(Token::LBrace)
            ),
            _ => false,
        }
    }

    /// Parse a record repr `{ F : T1; mutable G : T2 }` into a
    /// [`SyntaxKind::RECORD_REPR`] node (`SynTypeDefnSimpleRepr.Record`,
    /// `pars.fsy:2479`), with an optional leading repr-level access modifier.
    /// The caller has verified [`Self::peek_is_record_repr_start`]. Fields are
    /// `[mutable] ident : <typ>` separated by `;`/`OBLOCKSEP` runs (the
    /// anon-record-type `seps` machinery, phase 7.9); the closing `}` is
    /// LexFilter-swallowed, recovered from the raw stream by
    /// [`Self::bump_swallowed_closer`].
    pub(super) fn parse_record_repr(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::RECORD_REPR));
        // Optional repr-level access modifier — `type T = private { … }`
        // (`SynTypeDefnSimpleRepr.Record.accessibility`). Consumed as
        // `ACCESS_TOK` and elided by the normaliser, like other accessibility.
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
        self.bump_into(SyntaxKind::LBRACE_TOK);

        // The close `}` is LexFilter-swallowed, so probe the raw stream.
        let at_close =
            |p: &Self| matches!(p.next_non_trivia_raw_at_pos(), Some(Token::RBrace) | None);
        // A record must have at least one field; an empty `{ }` is an FCS error
        // (`braceFieldDeclList` has no empty production). The swallowed `}` is
        // not in the filtered stream, so "no field" surfaces as the cursor
        // already being at the (swallowed) close — i.e. not a field start.
        if self.raw_starts_record_field_decl() {
            self.parse_record_field_decl();
            // Subsequent fields after one `seps_block` group. FCS's `seps_block`
            // is a *single* group, so a repeated separator
            // (`type T = { F : int; ; G : int }`) is a parse error; consuming one
            // group per gap (via `consume_one_seps_group`) leaves any extra to
            // trip the field parser's recovery. A trailing group before the
            // swallowed `}` is tolerated; the loop gates on `at_close` (raw
            // `}`/EOF) *before* consuming so an enclosing scope's separator isn't
            // drained. Mirrors `parse_record_body`.
            while !at_close(self) && self.consume_one_seps_group(at_close) {
                if at_close(self) || !self.raw_starts_record_field_decl() {
                    break;
                }
                self.parse_record_field_decl();
            }
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected at least one field in record type".to_string(),
                span,
            });
        }

        // The record owns exactly one trailing `Virtual::BlockSep`: the offside
        // separator of the `}`-on-own-line layout (`type T = {⏎ X : int⏎ }`),
        // which sits *before* the swallowed `}`. Consume it zero-width —
        // advancing `pos` but not the raw cursor — so `bump_swallowed_closer`
        // still finds the raw `}` *and* the enclosing `parse_type_defn_repr`
        // still observes the type body's following `BlockEnd`, which it needs to
        // admit an `and` continuation (`type T = {⏎ … }⏎ and U = …`). A draining
        // `bump_into` would instead eat the `}`.
        //
        // A `BlockSep` sitting *after* the `}` is **not** the record's — it is
        // the type body's separator before a bare trailing member
        // (`type R =⏎ { … }⏎ member …`, phase 9.13b), owned by
        // `parse_type_defn_repr`'s bare-members hook. Leaving it there lets that
        // hook consume it (via `bump_into`) in its correct place *after* the
        // `}`, rather than mis-stamping it zero-width *inside* the record before
        // the closer. The span comparison against the swallowed `}` makes the
        // ownership local: eat iff the separator precedes the closer. When the
        // block has already closed (`type R = { X: int }` with the next decl
        // offside), the `BlockEnd` arrives first — no `BlockSep` at the cursor —
        // so this no-ops, leaving the enclosing scope's separator alone. Record
        // *expressions* / *patterns* have no `and` chain, so their loops
        // harmlessly leave this `BlockSep` for the caller; only the type-def
        // repr must consume its own here.
        let own_trailing_sep_start = match self.peek() {
            Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), sep_span)) => Some(sep_span.start),
            _ => None,
        };
        if own_trailing_sep_start.is_some_and(|sep_start| {
            matches!(
                self.next_non_trivia_raw_at_pos_with_span(),
                Some((Token::RBrace, brace_span)) if sep_start < brace_span.start
            )
        }) {
            self.eat_zero_width_virtual(Virtual::BlockSep);
        }

        // Closing `}` — swallowed by LexFilter, recovered from the raw stream.
        self.bump_swallowed_closer(
            SyntaxKind::RBRACE_TOK,
            |t| matches!(t, Token::RBrace),
            "}",
            "record type",
        );
        self.builder.finish_node(); // RECORD_REPR
    }

    /// `true` iff the cursor begins a record field — a leading attribute list
    /// `[<…>]` (phase 10.7), a `mutable` keyword, an access modifier, or a
    /// field-name ident. (The swallowed `}` close is not a field start, so this is
    /// `false` at the end of the field list.)
    fn raw_starts_record_field_decl(&self) -> bool {
        matches!(
            self.peek(),
            Some((
                Ok(FilteredToken::Raw(
                    Token::LBrackLess
                        | Token::Mutable
                        | Token::Internal
                        | Token::Private
                        | Token::Public
                        | Token::Ident(_)
                        | Token::QuotedIdent(_)
                )),
                _,
            ))
        )
    }

    /// Parse one record field into a [`SyntaxKind::RECORD_FIELD_DECL`] node —
    /// `[MUTABLE_TOK?, IDENT_TOK, COLON_TOK, <typ>]` (`fieldDecl`,
    /// `pars.fsy:2978`). FCS's `SynField.isMutable` is the `mutable` keyword;
    /// the field type is the full `parse_type`. A record field may **not** carry
    /// a visibility modifier (FCS errors `parsRecordFieldsCannotHaveVisibility`);
    /// we consume a stray access modifier as `ACCESS_TOK` and record that error
    /// so the field shape stays predictable.
    fn parse_record_field_decl(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::RECORD_FIELD_DECL));
        // Leading attribute lists (phase 10.7) — `{ [<A>] X : int }`, FCS's
        // `SynField.attributes` (field 0). They precede the `mutable` keyword and
        // the field name, as leading children of the `RECORD_FIELD_DECL`. An
        // attribute on its own line (`{ [<A>]⏎  X : int }`) leaves a trailing
        // offside `OBLOCKSEP`; drain it (the 10.7a header idiom) so the name check
        // sees `X`, not the virtual.
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
            }
        }
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Mutable)), _))
        ) {
            self.bump_into(SyntaxKind::MUTABLE_TOK);
        }
        if let Some((
            Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
            span,
        )) = self.peek().cloned()
        {
            self.errors.push(ParseError {
                message: "a record field may not have an accessibility modifier".to_string(),
                span,
            });
            self.bump_into(SyntaxKind::ACCESS_TOK);
        }
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
                message: "expected field name in record type".to_string(),
                span,
            });
        }
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
            self.bump_into(SyntaxKind::COLON_TOK);
        } else {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected `:` in record field".to_string(),
                span,
            });
        }
        self.parse_type();
        self.builder.finish_node(); // RECORD_FIELD_DECL
    }

    /// `true` iff the body begins a discriminated-union or enum repr
    /// (`pars.fsy:2714` `unionTypeRepr`), optionally preceded by a repr-level
    /// access modifier. The "head" (after the optional access) qualifies iff it
    /// is a leading `|`, or a case-name ident followed by `of` (union case),
    /// `= value` (enum case), or a `|` that is **not** immediately before `null`
    /// — the `Ident | null` shape is FCS's `WithNull` abbreviation (7.11), not a
    /// union (verified via `fcs-dump`). A bare `Ident` (with `.`/`<`/`*`/`->`/…
    /// after it, or nothing) is an abbreviation, so this returns `false` and the
    /// abbreviation arm takes it. The Union-vs-Enum choice is made post-hoc in
    /// [`Self::parse_union_or_enum_repr`].
    pub(super) fn peek_is_union_or_enum_repr_start(&self) -> bool {
        // Significant (non-trivia) raw tokens at/after the cursor. We are just
        // past `OBLOCKBEGIN` at a clean position (no partial split / swallowed
        // token between), so a direct raw walk suffices.
        let mut sig = self
            .raw_tokens
            .iter()
            .skip(self.raw_pos)
            .map_while(|(res, _)| res.as_ref().ok())
            .filter_map(|tt| match tt {
                TriviaToken::Lexed(t) => Some(t),
                _ => None,
            })
            .filter(|t| trivia_kind(t).is_none());
        let Some(first) = sig.next() else {
            return false;
        };
        // Skip an optional repr-level access modifier.
        let head = if matches!(first, Token::Internal | Token::Private | Token::Public) {
            match sig.next() {
                Some(t) => t,
                None => return false,
            }
        } else {
            first
        };
        match head {
            // Leading `|` — always a union.
            Token::Bar => true,
            // A case name. `of` → union; `= value` → enum; `: topType` → a
            // `FullType` union case (`type T = A : int -> T`, a bar-less first
            // case, FCS's `firstUnionCaseDeclOfMany`); `|` → union/enum unless
            // the next token is `null` (`WithNull` abbreviation). A top-level `:`
            // can only be a `FullType` case here — an abbreviation RHS
            // (`type T = int`) is a bare `typ`, which never continues with a `:`.
            Token::Ident(_) | Token::QuotedIdent(_) => match sig.next() {
                Some(Token::Of | Token::Equals | Token::Colon) => true,
                Some(Token::Bar) => !matches!(sig.next(), Some(Token::Null)),
                _ => false,
            },
            // A leading operator case name — `([])` / `( :: )` (FSharp.Core's
            // `list`), a bar-less first case. FCS admits a bar-less operator case
            // *only* through `unionCaseName COLON topType` (`pars.fsy:2855`): the
            // nullary / `of` operator forms need a leading `|` (`firstUnionCaseDecl`
            // uses a bare `ident` there). So require the full `( [] ) :` / `( :: ) :`
            // shape — the operator name, its closing `)` (on the raw stream this
            // walks, even though LexFilter later swallows it from the filtered
            // stream), then `:`. An ordinary parenthesised-type abbreviation
            // (`type T = (int)`) has an inner type after `(`, not `[`/`::`, so it
            // is unaffected.
            Token::LParen => {
                let closed = match sig.next() {
                    Some(Token::LBrack) => {
                        matches!(sig.next(), Some(Token::RBrack))
                            && matches!(sig.next(), Some(Token::RParen))
                    }
                    Some(Token::ColonColon) => matches!(sig.next(), Some(Token::RParen)),
                    _ => false,
                };
                closed && matches!(sig.next(), Some(Token::Colon))
            }
            _ => false,
        }
    }

    /// Parse a discriminated-union or enum repr (`SynTypeDefnSimpleRepr.Union` /
    /// `.Enum`, `pars.fsy:2461`): an optional repr-level access modifier, an
    /// optional leading `|`, then `Bar`-separated cases. Each case is parsed
    /// generically by [`Self::parse_union_or_enum_case`], which emits a
    /// [`SyntaxKind::UNION_CASE`] or [`SyntaxKind::ENUM_CASE`] per case; the
    /// enclosing node is then chosen post-hoc — `ENUM_REPR` if *any* case was an
    /// enum case (FCS's "any `Choice1Of2` ⇒ Enum"), else `UNION_REPR`. A *mixed*
    /// group (`A = 0 | B`) is FCS's `parsAllEnumFieldsRequireValues` error; we
    /// emit it (the value-less `UNION_CASE` then projects to no enum case, like
    /// FCS). The caller has verified [`Self::peek_is_union_or_enum_repr_start`].
    ///
    /// Returns whether the repr admits *bare* trailing members (phase 9.13b,
    /// the no-`with` `type U =⏎ | A⏎ member …` form): an enum always does, a
    /// union only when it carries at least one `|` — a zero-bar single case
    /// (`type U =⏎ X of int⏎ member …`) is FCS's "Unexpected keyword 'member'
    /// in type definition. Expected '|' or other token." (ground-truthed via
    /// `fcs-dump`; the bar-less case is parsed by a different yacc production
    /// whose follow set has no `classDefnMembers`).
    pub(super) fn parse_union_or_enum_repr(&mut self) -> bool {
        let cp = self.builder.checkpoint();
        // Optional repr-level access modifier. Valid on a *union*
        // (`type T = private A | B`), but **not** on an *enum*
        // (`type E = private A = 0` is FCS's
        // `parsEnumTypesCannotHaveVisibilityDeclarations`). Since Union-vs-Enum
        // is decided post-hoc, capture the span now and diagnose below if the
        // body turns out to be an enum. Elided by the normaliser either way.
        let repr_access_span = if let Some((
            Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
            span,
        )) = self.peek().cloned()
        {
            self.bump_into(SyntaxKind::ACCESS_TOK);
            Some(span)
        } else {
            None
        };
        // Optional leading `|`.
        let mut saw_bar = false;
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Bar)), _))) {
            self.bump_into(SyntaxKind::BAR_TOK);
            saw_bar = true;
        }
        let mut enum_cases = 0u32;
        // Spans of the value-less (union) cases, so a mixed group can anchor its
        // diagnostic on each offending case rather than at EOF (see below).
        let mut union_case_spans: Vec<Range<usize>> = Vec::new();
        self.parse_union_or_enum_case(&mut enum_cases, &mut union_case_spans);
        while matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Bar)), _))) {
            self.bump_into(SyntaxKind::BAR_TOK);
            saw_bar = true;
            self.parse_union_or_enum_case(&mut enum_cases, &mut union_case_spans);
        }
        // Post-hoc Union-vs-Enum decision. Any enum (`= value`) case ⇒ Enum; a
        // mix with a value-less case is FCS's `parsAllEnumFieldsRequireValues`,
        // reported once per offending case at that case's range (`pars.fsy:2471`
        // uses each `SynUnionCase.range`) — not a single span at EOF, so the LSP
        // points the user at the case that needs a value.
        let is_enum = enum_cases > 0;
        if is_enum {
            for span in &union_case_spans {
                self.errors.push(ParseError {
                    message: "all enum cases must be given values".to_string(),
                    span: span.clone(),
                });
            }
        }
        // An enum may not carry repr-level visibility (FCS errors but still
        // produces the `Enum`); a union may. Diagnose now that the kind is known.
        if is_enum && let Some(span) = repr_access_span {
            self.errors.push(ParseError {
                message: "accessibility modifiers are not permitted on enum types".to_string(),
                span,
            });
        }
        let kind = if is_enum {
            SyntaxKind::ENUM_REPR
        } else {
            SyntaxKind::UNION_REPR
        };
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(kind));
        self.builder.finish_node();
        is_enum || saw_bar
    }

    /// Parse one union or enum case. The case name is followed by either
    /// `= value` (an enum case, `pars.fsy:2785` → [`SyntaxKind::ENUM_CASE`],
    /// `enum_cases += 1`) or an optional `of T1 * …` field list (a union case,
    /// `pars.fsy:2846` → [`SyntaxKind::UNION_CASE`], whose case span is pushed
    /// to `union_case_spans`); the node kind is chosen after the `=`/`of` is
    /// seen, via a checkpoint. The case name is a single ident (operator names
    /// like `(::)` are a later slice). `union_case_spans` carries each union
    /// case's name span out so a mixed enum group can anchor its
    /// `parsAllEnumFieldsRequireValues` diagnostic on the offending case.
    fn parse_union_or_enum_case(
        &mut self,
        enum_cases: &mut u32,
        union_case_spans: &mut Vec<Range<usize>>,
    ) {
        let cp = self.builder.checkpoint();
        // Leading attribute lists (phase 10.7) — `| [<A>] X` / `| [<A>] A = 0`.
        // FCS's `SynUnionCase.attributes` / `SynEnumCase.attributes` (field 0).
        // The caller's loop has already consumed the `|` bar, so the attrs sit at
        // the cursor; parsed under `cp`, they become leading children of the
        // wrapped `UNION_CASE`/`ENUM_CASE`. (A first case attributed without a
        // leading `|` is an FCS parse error, and our repr dispatch likewise never
        // reaches here without a bar.) An attribute on its own line
        // (`| [<A>]⏎  X`) leaves a trailing offside `OBLOCKSEP`; drain it (the
        // 10.7a header idiom) so the case-name check sees `X`, not the virtual.
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
            }
        }
        // A union/enum case may *not* carry an accessibility modifier, but FCS's
        // `attrUnionCaseDecl` still consumes `opt_access` and reports it as not
        // permitted before recovering with the case. Mirror that — consume the
        // stray `private`/`internal`/`public` as `ACCESS_TOK` (+ a diagnostic)
        // so the case name still parses (`type T = A | private B` keeps `B`).
        if let Some((
            Ok(FilteredToken::Raw(Token::Internal | Token::Private | Token::Public)),
            span,
        )) = self.peek().cloned()
        {
            self.errors.push(ParseError {
                message: "accessibility modifiers are not permitted on union cases".to_string(),
                span,
            });
            self.bump_into(SyntaxKind::ACCESS_TOK);
        }
        // The case name's span — carried out for union cases so a mixed-enum
        // diagnostic can point at the case (FCS uses the `SynUnionCase.range`;
        // for a nullary case that is exactly the name).
        let case_span =
            if let Some((Ok(FilteredToken::Raw(Token::Ident(_) | Token::QuotedIdent(_))), span)) =
                self.peek().cloned()
            {
                self.bump_into(SyntaxKind::IDENT_TOK);
                span
            } else if let Some(lparen) = self.peek_union_op_case_name() {
                // Operator case name — `([])` (`op_Nil`) / `( :: )`
                // (`op_ColonColon`), FCS's `unionCaseName` `LPAREN LBRACK RBRACK
                // rparen` / `LPAREN COLON_COLON rparen` (`pars.fsy:2810`), the
                // FSharp.Core `list` constructors. The closing `)` is
                // LexFilter-swallowed (a paren closer); recovered like every
                // swallowed `)`.
                let start = lparen.start;
                self.parse_union_op_case_name();
                // Span the *whole* name (`(` through the recovered `)`, now at
                // `raw_consumed_end`) so a mixed-enum diagnostic anchors on the
                // case name rather than just the opening `(`.
                start..self.raw_consumed_end
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected union or enum case name".to_string(),
                    span: span.clone(),
                });
                span
            };

        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Equals)), _))
        ) {
            // Enum case `Name = <value>`. Drain the space after `=` first so the
            // value node's range stays tight (the expression parser does not
            // self-drain; there is no `OBLOCKBEGIN` bump to do it mid-case).
            self.bump_into(SyntaxKind::EQUALS_TOK);
            if let Some((_, span)) = self.peek() {
                let start = span.start;
                self.drain_raw_up_to(start);
            }
            self.parse_enum_case_value();
            *enum_cases += 1;
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::ENUM_CASE));
            self.builder.finish_node();
            return;
        }

        // Union case `FullType` form — `Name : topType` (`pars.fsy:2778`,
        // `SynUnionCaseKind.FullType`), FSharp.Core's `Option`/`Choice`
        // representation (`| None : 'T option`, `| Some : Value:'T -> 'T option`).
        // The case carries a *type signature* (a `topType`, so labelled
        // parameters `Value:'T` are admitted via [`Self::parse_top_type`]) in
        // place of the `of`-field list. Mutually exclusive with `of` (the `:` and
        // `of` are alternative `unionCaseRepr` heads), so handled here as its own
        // branch.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Colon)), _))) {
            self.bump_into(SyntaxKind::COLON_TOK);
            self.parse_top_type();
            union_case_spans.push(case_span);
            self.builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::UNION_CASE));
            self.builder.finish_node();
            return;
        }

        // Union case — an optional `of T1 * T2 * …` field list (shared with the
        // exception-definition case data, phase 9.15a).
        self.parse_opt_union_case_of_fields();
        union_case_spans.push(case_span);
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::UNION_CASE));
        self.builder.finish_node();
    }

    /// `Some(span)` (covering `(` through the operator) when the cursor opens an
    /// operator union-case name — `([])` or `( :: )` (FCS's `unionCaseName`
    /// operator forms, `pars.fsy:2810`). The `(` is a real filtered token; the
    /// immediately-following raw token is `[` (→ `[]`, `op_Nil`) or `::` (→ `::`,
    /// `op_ColonColon`). `None` for any other token (including an ordinary
    /// parenthesised expression, which a union case never is).
    fn peek_union_op_case_name(&self) -> Option<Range<usize>> {
        let (Ok(FilteredToken::Raw(Token::LParen)), lparen) = self.peek()? else {
            return None;
        };
        match self.next_non_trivia_raw_after(lparen.end) {
            Some(Token::LBrack | Token::ColonColon) => Some(lparen.clone()),
            _ => None,
        }
    }

    /// Parse an operator union-case name — `([])` (`op_Nil`) or `( :: )`
    /// (`op_ColonColon`) — into the open node as `[LPAREN_TOK, (LBRACK_TOK
    /// RBRACK_TOK | COLON_COLON_TOK), RPAREN_TOK]`. The caller has verified
    /// [`Self::peek_union_op_case_name`]. The closing `)` is LexFilter-swallowed
    /// (a paren closer) and recovered via [`Self::bump_swallowed_rparen`]; the
    /// normaliser maps the bracket / `::` tokens to FCS's `op_Nil` /
    /// `op_ColonColon` `idText`.
    fn parse_union_op_case_name(&mut self) {
        self.bump_into(SyntaxKind::LPAREN_TOK);
        match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::LBrack)), _)) => {
                self.bump_into(SyntaxKind::LBRACK_TOK);
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
                        message: "expected `]` in the `[]` union case name".to_string(),
                        span,
                    });
                }
            }
            Some((Ok(FilteredToken::Raw(Token::ColonColon)), _)) => {
                self.bump_into(SyntaxKind::COLON_COLON_TOK);
            }
            _ => {}
        }
        self.bump_swallowed_rparen(SyntaxKind::RPAREN_TOK);
    }

    /// Parse an optional `of T1 * T2 * …` union-case field list at the cursor,
    /// emitting the `of` keyword as [`SyntaxKind::OF_TOK`], each field via
    /// [`Self::parse_union_case_field`], and `*` separators as
    /// [`SyntaxKind::STAR_TOK`]. A no-op if no `of` follows. Shared by a
    /// discriminated-union case ([`Self::parse_union_or_enum_case`]) and an
    /// exception definition's case data ([`Self::parse_exception_defn`], which
    /// reuses `SynUnionCase` for `SynExceptionDefnRepr.caseName`). An `of` with
    /// no following type (`type T = A of` / `exception E of`) is a recoverable
    /// error, not a panic: each field is gated on a type-starter (FCS recovers
    /// with zero fields).
    fn parse_opt_union_case_of_fields(&mut self) {
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::Of)), _))) {
            self.bump_into(SyntaxKind::OF_TOK);
            if self.peek_starts_type_or_anon_recd() {
                self.parse_union_case_field();
                while self
                    .next_non_trivia_raw_at_pos()
                    .is_some_and(|t| matches!(t, Token::Op("*")))
                    && matches!(
                        self.peek(),
                        Some((Ok(FilteredToken::Raw(Token::Op("*"))), _))
                    )
                {
                    self.bump_into(SyntaxKind::STAR_TOK);
                    if !self.peek_starts_type_or_anon_recd() {
                        let span = self
                            .peek()
                            .map(|(_, s)| s.clone())
                            .unwrap_or_else(|| self.source.len()..self.source.len());
                        self.errors.push(ParseError {
                            message: "expected a union case field after `*`".to_string(),
                            span,
                        });
                        break;
                    }
                    self.parse_union_case_field();
                }
            } else {
                let span = self
                    .peek()
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| self.source.len()..self.source.len());
                self.errors.push(ParseError {
                    message: "expected a field type after `of`".to_string(),
                    span,
                });
            }
        }
    }

    /// Parse an exception definition (phase 9.15a) into a
    /// [`SyntaxKind::EXCEPTION_DEFN`] node — FCS's `SynModuleDecl.Exception(
    /// SynExceptionDefn(SynExceptionDefnRepr(attrs, caseName, longId, …),
    /// withKeyword, members, …), …)` (`SyntaxTree.fsi:1771`). Shape
    /// `[EXCEPTION_TOK, ACCESS_TOK?, UNION_CASE, (EQUALS_TOK · <path>)?]`:
    ///
    /// * `exception` reaches the parser as a real filtered `Token::Exception`
    ///   (LexFilter opens the *silent* `CtxtException` but passes the keyword
    ///   through — it is not swallowed like `type`/`module`), so the caller
    ///   ([`Self::parse_module_decls`]) dispatched here on a `peek()` of it.
    /// * An optional accessibility modifier is **permitted** here
    ///   (`exception internal E`, FCS's `exconCore` `opt_access` →
    ///   `SynExceptionDefnRepr.accessibility`), unlike on a union case; it is a
    ///   direct `ACCESS_TOK` child (a sibling of the case, mirroring FCS) and is
    ///   elided by the normaliser.
    /// * The case data (`exconIntro`: `ident [of fields]`) reuses the phase-9.5
    ///   [`SyntaxKind::UNION_CASE`] machinery — `SynExceptionDefnRepr.caseName`
    ///   is a `SynUnionCase`.
    /// * An optional `= path` abbreviation (`exconRepr`) fills
    ///   `SynExceptionDefnRepr.longId`; the path reuses
    ///   [`Self::parse_long_ident_path`].
    /// * An optional `with member …` augmentation (`opt_classDefn`, phase 9.15b)
    ///   fills `SynExceptionDefn.withKeyword`/`members`. The members land in the
    ///   **outer** `members` slot — direct [`SyntaxKind::MEMBER_DEFN`] children of
    ///   the `EXCEPTION_DEFN`, after a marker [`SyntaxKind::WITH_TOK`] — via the
    ///   `with`-augment helper ([`Self::parse_with_augmentation_members`]) shared
    ///   with the phase-9.13a type augmentation.
    pub(super) fn parse_exception_defn(&mut self) {
        self.parse_exception_defn_at(None);
    }

    /// Parse an exception definition. With `cp = None` this is the plain form;
    /// with `cp = Some(checkpoint)` the caller has already emitted one or more
    /// leading `ATTRIBUTE_LIST`s (phase 10.7m) after the checkpoint, and this
    /// wraps them — together with the definition — so the attributes become
    /// leading children of the `EXCEPTION_DEFN` (FCS attaches a leading
    /// `[<A>] exception …` attribute to `SynExceptionDefnRepr.attributes`, `$1`).
    /// Any *after-keyword* attributes (`exception [<B>] …`, FCS's `EXCEPTION
    /// opt_attributes …` = `cas`) are parsed below and join the same slot, in
    /// source order — matching FCS's `$1 @ cas`. Mirrors
    /// [`Self::parse_type_defn_at`], but `exception` is a real filtered token (not
    /// swallowed by LexFilter), so the caller skips the inter-line `BlockSep`
    /// itself and we simply `bump_into` the keyword.
    pub(super) fn parse_exception_defn_at(&mut self, cp: Option<rowan::Checkpoint>) {
        match cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::EXCEPTION_DEFN)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::EXCEPTION_DEFN)),
        }
        // The shared `exconCore` (`pars.fsy:exconCore`): `exception` keyword,
        // after-keyword attributes, optional accessibility, the case, and the
        // optional `= path` abbreviation.
        self.parse_exception_repr_core();
        // Optional `with member …` augmentation (`opt_classDefn`, phase 9.15b).
        // The `with` is a raw token (FCS's `SynExceptionDefn.withKeyword`); the
        // members land in the *outer* `SynExceptionDefn.members` slot, so the
        // `WITH_TOK` and the member nodes are direct children of `EXCEPTION_DEFN`
        // (no repr — unlike the type augmentation's `OBJECT_MODEL_REPR` marker).
        // The filtered stream after the `with` is identical to the 9.13a type
        // augmentation, so the member loop + close-virtual drain are shared.
        if matches!(self.peek(), Some((Ok(FilteredToken::Raw(Token::With)), _))) {
            self.bump_into(SyntaxKind::WITH_TOK);
            self.parse_with_augmentation_members(false, true);
        }
        self.builder.finish_node(); // EXCEPTION_DEFN
    }

    /// Parse the shared exception `exconCore` (`pars.fsy:1101`) into the
    /// *currently open* [`SyntaxKind::EXCEPTION_DEFN`] node — the `exception`
    /// keyword, after-keyword attributes, optional accessibility, the case
    /// (`ident [of fields]` → [`SyntaxKind::UNION_CASE`]), and the optional
    /// `= path` abbreviation. Shared by the impl exception definition
    /// ([`Self::parse_exception_defn_at`], whose `opt_classDefn` `with`-augment of
    /// member *bodies* follows) and the signature exception
    /// ([`Self::parse_sig_exception_defn_at`], phase 10.15, whose `opt_classSpfn`
    /// `with`-augment of member *sigs* is a later slice) — FCS's `exconDefn` and
    /// `exconSpfn` both begin `exconCore`, identically.
    fn parse_exception_repr_core(&mut self) {
        // The `exception` keyword.
        self.bump_into(SyntaxKind::EXCEPTION_TOK);
        // After-keyword attributes (`exception [<B>] E`, FCS's `EXCEPTION
        // opt_attributes opt_access exconIntro`). They sit between `EXCEPTION_TOK`
        // and the case — before the `UNION_CASE` checkpoint below — so they are
        // direct `EXCEPTION_DEFN` children (not the case's), sharing the repr's
        // `attributes` slot with any leading `[<A>] exception …` lists wrapped via
        // the caller's `cp`. The grammar licenses these only here, immediately
        // after the keyword and before `opt_access`/the case name.
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::LBrackLess)), _))
        ) {
            self.parse_attribute_lists();
            // Absorb the attribute list's trailing `opt_OBLOCKSEP` — the offside
            // `exception [<A>]⏎E` / `exception [<A>]⏎internal E` form, where the
            // name (or `internal`/`private`/`public`) sits on a fresh line. FCS's
            // `attributeList` swallows this `OBLOCKSEP` (`ParseHadErrors: false`),
            // so the definition continues on the next line; without the drain the
            // `BlockSep` would be mis-bumped as the name. Gated on the attribute's
            // presence — a bare `exception⏎E` has no separator to license the
            // column-0 layout and is an FCS error.
            while matches!(
                self.peek(),
                Some((Ok(FilteredToken::Virtual(Virtual::BlockSep)), _))
            ) {
                self.bump_into(SyntaxKind::ERROR);
            }
        }
        // Optional accessibility (`exception internal E`) — valid on an
        // exception. Captured as a sibling `ACCESS_TOK` of the case (elided).
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
        // The case: `ident [of fields]` → UNION_CASE. The checkpoint is taken
        // *before* the name so the name's leading trivia (drained by
        // `bump_into`) lands inside the case node, matching the 9.5 shape.
        let cp = self.builder.checkpoint();
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
                message: "expected an exception name".to_string(),
                span,
            });
        }
        self.parse_opt_union_case_of_fields();
        self.builder
            .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::UNION_CASE));
        self.builder.finish_node(); // UNION_CASE
        // Optional `= path` abbreviation (`exconRepr: EQUALS path`). The `=`
        // here is the abbreviation target, not an enum value — which is why the
        // case data above reuses `parse_opt_union_case_of_fields` rather than
        // the full `parse_union_or_enum_case` (whose `=` arm is an enum case).
        if matches!(
            self.peek(),
            Some((Ok(FilteredToken::Raw(Token::Equals)), _))
        ) {
            self.bump_into(SyntaxKind::EQUALS_TOK);
            self.parse_long_ident_path("exception abbreviation");
        }
    }

    /// Parse an exception *signature* (phase 10.15) into the impl-side
    /// [`SyntaxKind::EXCEPTION_DEFN`] node —
    /// `SynModuleSigDecl.Exception(SynExceptionSig, range)`. FCS's `exconSpfn`
    /// (`pars.fsy:1112`) is `exconCore opt_classSpfn`; `exconCore` is shared with
    /// the impl exception (so the [`Self::parse_exception_repr_core`] node, facade,
    /// and `NormalisedExnDefn` projection are reused). The `with member …`
    /// augmentation (`opt_classSpfn`, whose members are member *sigs* rather than
    /// the impl's member bodies) parses via [`Self::parse_with_augmentation_members`]
    /// with `sig = true` — the member sigs land as `MEMBER_SIG` children of the
    /// outer `members` slot. Caller has verified the cursor is at the raw
    /// `exception` keyword.
    pub(super) fn parse_sig_exception_defn(&mut self) {
        self.parse_sig_exception_defn_at(None);
    }

    /// As [`Self::parse_sig_exception_defn`], but with `cp = Some(checkpoint)` the
    /// caller has already emitted one or more leading `ATTRIBUTE_LIST`s (the
    /// attributed sig form `[<A>] exception E`) after the checkpoint; the
    /// `EXCEPTION_DEFN` is opened *at* that checkpoint so the attribute lists
    /// become its leading children (FCS homes them in
    /// `SynExceptionDefnRepr.attributes`). `None` is the plain form.
    pub(super) fn parse_sig_exception_defn_at(&mut self, cp: Option<rowan::Checkpoint>) {
        match cp {
            Some(cp) => self
                .builder
                .start_node_at(cp, FSharpLang::kind_to_raw(SyntaxKind::EXCEPTION_DEFN)),
            None => self
                .builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::EXCEPTION_DEFN)),
        }
        self.parse_exception_repr_core();
        // The `with member …` augmentation (`opt_classSpfn`, phase 10.15 second
        // slice). Two LexFilter forms:
        //  * `Raw(with) OBLOCKBEGIN … OBLOCKEND` — every *supported* member-sig
        //    start (`member`/`static member`/`abstract`/…). The member sigs land in
        //    the outer `SynExceptionSig.members` slot as direct `MEMBER_SIG`
        //    children after the `WITH_TOK` (no repr — mirroring the impl exception,
        //    but member sigs not bodies), via `parse_with_augmentation_members`
        //    with `sig = true`.
        //  * `OWITH … OEND` (`Virtual::With`) — emitted when the first augment
        //    member begins on the *same line* with a leading `[<…>]` attribute or
        //    access modifier (`exception E with [<A>] member …`). Those augment
        //    forms are a later slice, so the block is contained as ERROR rather
        //    than left to escape the `EXCEPTION_DEFN`.
        enum WithForm {
            None,
            Raw,
            Owith(std::ops::Range<usize>),
        }
        let form = match self.peek() {
            Some((Ok(FilteredToken::Raw(Token::With)), _)) => WithForm::Raw,
            Some((Ok(FilteredToken::Virtual(Virtual::With)), sp)) => WithForm::Owith(sp.clone()),
            _ => WithForm::None,
        };
        match form {
            WithForm::Raw => {
                self.bump_into(SyntaxKind::WITH_TOK);
                self.parse_with_augmentation_members(true, true);
            }
            WithForm::Owith(span) => {
                self.errors.push(ParseError {
                    message: "this member signature in a `with` augmentation is not yet \
                              supported (later phase-10 slice)"
                        .to_string(),
                    span,
                });
                self.skip_owith_block_as_error();
            }
            WithForm::None => {}
        }
        self.builder.finish_node(); // EXCEPTION_DEFN
    }

    /// Parse an enum case's value — the `<value>` of `Name = <value>` — after
    /// `=` and its trailing trivia have been consumed. FCS's enum value is
    /// `atomicExpr` (`pars.fsy:2785 unionCaseName EQUALS atomicExpr`), a rich,
    /// self-recursive nonterminal. We parse the realistic constant surface
    /// faithfully and bound the exotic forms:
    ///
    /// * **Literal / dotted long-ident** (`0`, `'a'`, `System.Int32.MaxValue`):
    ///   the plain atomic head ([`Self::parse_atomic_expr`], whose
    ///   [`Self::parse_ident_expr`] handles the dotted path).
    /// * **Adjacent-signed numeric literal** (`-1`, `+1`): the sign-fold pass
    ///   ([`super::sign_fold`], run before the parser) has already merged an
    ///   adjacent sign on a foldable numeric literal into a single signed
    ///   literal token, exactly as FCS folds at the token layer. So a folded
    ///   value reaches here as an ordinary literal and flows through the atomic
    ///   path above into a `Const`, matching FCS — no special handling. Any
    ///   `+`/`-` still present as an `Op` token therefore did **not** fold (a
    ///   *spaced* sign `- 1`, a non-numeric operand `-foo`, or an unsigned
    ///   suffix `-1uy`), and FCS rejects all of those as a non-`atomicExpr`
    ///   value (`Unexpected symbol`/`prefix operator in union case`); we reject
    ///   them too, consuming the stray sign as a zero-noise `ERROR`.
    /// * **High-precedence paren application** (`f(1)`): `atomicExpr` is
    ///   self-recursive on `HIGH_PRECEDENCE_PAREN_APP` (`pars.fsy:5247`), so we
    ///   consume it as part of the value (the same `APP_EXPR > [head, ⟨HPA
    ///   marker⟩, paren]` shape [`Self::parse_app_expr`] builds), matching FCS.
    ///
    /// Documented divergences, all in the *permissive* (no false-positive)
    /// direction or bounded by parser-wide limits:
    /// * Chained `f(1)(2)` is accepted; FCS rejects it as a quirk of its LR
    ///   tables. The surplus application is invalid code that fails later.
    /// * The full `atomicExpr` postfix tail folds in here: the high-precedence
    ///   bracket indexer `f[1]` (`App(Atomic, f, [1])`, phase 10.16c), the
    ///   type application `f<…>` (phase 10.21), and dot/index access — all parse
    ///   into the value rather than dangling into the enclosing repr loop,
    ///   matching FCS (which also accepts these here and rejects them later as
    ///   non-constant).
    fn parse_enum_case_value(&mut self) {
        if let Some((Ok(FilteredToken::Raw(Token::Op("-" | "+"))), sign_span)) =
            self.peek().cloned()
        {
            // A `+`/`-` that survived the sign-fold pass as an `Op` token did not
            // fold (spaced, non-numeric operand, or unsigned suffix), so it is a
            // non-`atomicExpr` value FCS rejects. Consume the stray sign as a
            // zero-noise `ERROR` inside the case so it is not re-reported by the
            // enclosing recovery; the operand falls through to the module loop,
            // mirroring FCS keeping the failed value in the case.
            self.errors.push(ParseError {
                message: "expected a constant enum value; a `-`/`+` sign here folds only \
                          when adjacent to a numeric literal"
                    .to_string(),
                span: sign_span,
            });
            self.bump_into(SyntaxKind::ERROR);
            return;
        }
        if !self.peek_starts_atomic_expr() {
            let span = self
                .peek()
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| self.source.len()..self.source.len());
            self.errors.push(ParseError {
                message: "expected an atomic value after `=` in enum case".to_string(),
                span,
            });
            return;
        }
        // The enum value is a full `atomicExpr`: [`Self::parse_atomic_expr`]'s
        // postfix tail now folds in the high-precedence paren applications
        // (`type E = A = f(1)` → `App(Atomic, …)`) and any dot/index access, so
        // no separate loop is needed here.
        self.parse_atomic_expr();
    }

    /// Parse one union-case field into a [`SyntaxKind::UNION_CASE_FIELD`] node
    /// (`pars.fsy:2922`/`unionCaseReprElement`): an optional `name :` prefix
    /// (FCS's `SynField.idOpt`) then the field type. The type is FCS's
    /// `appTypeNullableInParens`, i.e. [`Self::parse_app_type`] — **not** the
    /// can-be-nullable variant: an unparenthesised `T | null` is rejected by FCS
    /// (the `| null` is only nullable when parenthesised, `(T | null)`), so the
    /// bare `|` must terminate the field rather than be absorbed. `parse_app_type`
    /// also stops at the enclosing `*`, so the `*` separates *fields*.
    fn parse_union_case_field(&mut self) {
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::UNION_CASE_FIELD));
        // Named field `name : T` — an ident immediately followed by a colon.
        if let Some((Token::Ident(_) | Token::QuotedIdent(_), id_span)) =
            self.next_non_trivia_raw_at_pos_with_span()
            && matches!(
                self.next_non_trivia_raw_after(id_span.end),
                Some(Token::Colon)
            )
        {
            self.bump_into(SyntaxKind::IDENT_TOK);
            self.bump_into(SyntaxKind::COLON_TOK);
        }
        // Gate the type parse so a missing type after `name :` (`A of x :`) is a
        // recoverable error, not a panic in `parse_atomic_type`.
        if self.peek_starts_type_or_anon_recd() {
            // Drain the leading separator trivia (the space after `of`/`:`/`*`)
            // as a sibling *before* the type node, so the type's range stays
            // tight — `parse_app_type` (unlike `parse_type`) does not self-drain.
            // Mirrors the `open type` direct `parse_app_type` caller.
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
                message: "expected a union case field type".to_string(),
                span,
            });
        }
        self.builder.finish_node(); // UNION_CASE_FIELD
    }
}
