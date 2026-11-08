//! Keywords that reach the parser in one of two spellings: LexFilter's offside
//! relabel, or the raw token.
//!
//! FCS's grammar gives each of these keywords two sibling alternatives in the
//! same production: `THEN declExpr` beside `OTHEN typedSequentialExprBlock`,
//! `WITH withPatternClauses` beside `OWITH withPatternClauses OEND`, `FUN … RARROW
//! typedSequentialExprBlock` beside `OFUN … RARROW typedSequentialExprBlockR
//! OEND`, and so on (`pars.fsy`). The lex filter picks the spelling: it relabels
//! the keyword when it pushes the matching offside context, and passes it through
//! raw when it pushes none. That happens inside a module header's attribute list
//! (`module [<A(fun x -> x)>] M`), where FCS's lex filter suspends context pushes.
//!
//! The parser has no `#light "off"` mode, so it accepts the raw spelling wherever
//! it accepts the relabel, as FCS's grammar does, and leaves it to the lex filter
//! to decide where each appears. [`DualKeyword::form_of`] is the single
//! classifier: an expression-start gate and the matching dispatch arm both ask it,
//! so the gate cannot admit a spelling that the dispatch then fails to recognise.

use super::*;

/// A keyword that the lex filter either relabels to an offside [`Virtual`]
/// (backed by the raw token at the same span) or passes through raw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DualKeyword {
    /// `then` — `OTHEN` or `THEN`.
    Then,
    /// `else` — `OELSE` or `ELSE`.
    Else,
    /// `with` (of `match`/`try`) — `OWITH` or `WITH`.
    With,
    /// `fun` — `OFUN` or `FUN`.
    Fun,
    /// `function` — `OFUNCTION` or `FUNCTION`.
    Function,
    /// `do` (of a `while`/`for` loop) — `ODO` or `DO`.
    Do,
}

/// Which spelling a [`DualKeyword`] arrived in. It decides which closing
/// virtuals the construct owns: an [`Offside`](Self::Offside) keyword's context
/// pops with an `OEND`/`ORIGHT_BLOCK_END`, a [`Raw`](Self::Raw) keyword pushed no
/// context, so nothing closes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeywordForm {
    /// LexFilter's relabel ([`Virtual::Then`], [`Virtual::With`], …).
    Offside,
    /// The raw token, with no offside context pushed for it.
    Raw,
}

impl DualKeyword {
    /// The offside relabel of this keyword.
    fn offside(self) -> Virtual {
        match self {
            DualKeyword::Then => Virtual::Then,
            DualKeyword::Else => Virtual::Else,
            DualKeyword::With => Virtual::With,
            DualKeyword::Fun => Virtual::Fun,
            DualKeyword::Function => Virtual::Function,
            DualKeyword::Do => Virtual::Do,
        }
    }

    /// `true` if `tok` is this keyword's raw token.
    fn is_raw(self, tok: &Token<'_>) -> bool {
        matches!(
            (self, tok),
            (DualKeyword::Then, Token::Then)
                | (DualKeyword::Else, Token::Else)
                | (DualKeyword::With, Token::With)
                | (DualKeyword::Fun, Token::Fun)
                | (DualKeyword::Function, Token::Function)
                | (DualKeyword::Do, Token::Do)
        )
    }

    /// The green-tree kind the keyword's text is emitted under, in either
    /// spelling.
    fn syntax_kind(self) -> SyntaxKind {
        match self {
            DualKeyword::Then => SyntaxKind::THEN_TOK,
            DualKeyword::Else => SyntaxKind::ELSE_TOK,
            DualKeyword::With => SyntaxKind::WITH_TOK,
            DualKeyword::Fun => SyntaxKind::FUN_TOK,
            DualKeyword::Function => SyntaxKind::FUNCTION_TOK,
            DualKeyword::Do => SyntaxKind::DO_TOK,
        }
    }

    /// The spelling `tok` carries this keyword in, or `None` if it is not this
    /// keyword.
    pub(super) fn form_of(self, tok: &FilteredToken<'_>) -> Option<KeywordForm> {
        match tok {
            FilteredToken::Virtual(v) if *v == self.offside() => Some(KeywordForm::Offside),
            FilteredToken::Raw(t) if self.is_raw(t) => Some(KeywordForm::Raw),
            _ => None,
        }
    }

    /// The lambda keyword (`fun` or `function`, either spelling) `tok` is, if
    /// any. Every expression-start gate and the `parse_minus_expr` dispatch
    /// classify a lambda head through this one function.
    pub(super) fn lambda_head(tok: &FilteredToken<'_>) -> Option<DualKeyword> {
        [DualKeyword::Fun, DualKeyword::Function]
            .into_iter()
            .find(|kw| kw.form_of(tok).is_some())
    }
}

impl<'src> Parser<'src> {
    /// Consume `kw` in either spelling, emitting its text under the keyword's
    /// kind, and return the spelling; `None` (consuming nothing) if the current
    /// token is not `kw`.
    ///
    /// The offside relabel is backed by the raw token at the same span, so the
    /// raw text is emitted directly and both cursors advance (going through
    /// [`Self::bump_into`] would drain the raw keyword as an ERROR before
    /// reaching the virtual). The raw spelling is an ordinary 1:1 bump.
    pub(super) fn bump_dual_keyword(&mut self, kw: DualKeyword) -> Option<KeywordForm> {
        let (form, span) = match self.peek() {
            Some((Ok(tok), span)) => (kw.form_of(tok)?, span.clone()),
            _ => return None,
        };
        match form {
            KeywordForm::Offside => {
                self.drain_raw_up_to(span.start);
                debug_assert!(
                    matches!(
                        self.raw_tokens.get(self.raw_pos),
                        Some((Ok(TriviaToken::Lexed(t)), s)) if kw.is_raw(t) && *s == span,
                    ),
                    "{kw:?}'s offside relabel must be backed by its raw token at raw_pos with \
                     a matching span"
                );
                self.emit_text(kw.syntax_kind(), span);
                self.raw_pos += 1;
                self.pos += 1;
            }
            KeywordForm::Raw => self.bump_into(kw.syntax_kind()),
        }
        Some(form)
    }
}
