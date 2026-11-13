//! The productions' cursor over the filtered token table.
//!
//! The table stores kinds and spans and rebuilds a token on each read
//! ([`FilteredTokens::get`]). The productions read the token at the cursor many
//! times for every token they consume, so the cursor rebuilds it once per move
//! and lends it out ([`FilteredCursor::current`]). Its fields are private to
//! this module, so the only way to move the cursor is through the methods that
//! keep the lent token in step with the position.

use super::FilteredTok;
use crate::tokens::FilteredTokens;

pub(super) struct FilteredCursor<'src> {
    tokens: FilteredTokens<'src>,
    pos: usize,
    /// `tokens.get(pos)`.
    current: Option<FilteredTok<'src>>,
}

impl<'src> FilteredCursor<'src> {
    /// A cursor at the first token of `tokens`.
    pub(super) fn new(tokens: FilteredTokens<'src>) -> Self {
        let current = tokens.get(0);
        FilteredCursor {
            tokens,
            pos: 0,
            current,
        }
    }

    /// The table the cursor walks.
    pub(super) fn tokens(&self) -> &FilteredTokens<'src> {
        &self.tokens
    }

    /// The table the cursor walks, consuming the cursor.
    pub(super) fn into_tokens(self) -> FilteredTokens<'src> {
        self.tokens
    }

    /// The index of the token at the cursor.
    pub(super) fn pos(&self) -> usize {
        self.pos
    }

    /// The token at the cursor, or `None` at the end.
    pub(super) fn current(&self) -> Option<&FilteredTok<'src>> {
        self.current.as_ref()
    }

    /// Move the cursor to the next token; past the last one there is no
    /// token at the cursor.
    pub(super) fn advance(&mut self) {
        self.pos += 1;
        self.current = self.tokens.get(self.pos);
    }

    /// Move the cursor past the last token.
    pub(super) fn advance_to_end(&mut self) {
        self.pos = self.tokens.len();
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language_version::LanguageVersion;
    use crate::lexfilter::filter_collect;
    use crate::tokens::lex_full_trivia;
    use std::collections::HashSet;

    /// Walking the cursor to the end and past it lends exactly the table's
    /// token at each position.
    #[test]
    fn the_lent_token_is_the_table_token_at_every_position() {
        let source = "let f x =\n  match x with\n  | -1 -> f<int> (x)\n  | _ -> 0\n";
        let lexed = lex_full_trivia(source, &HashSet::new());
        let tokens = filter_collect(source, LanguageVersion::Preview, lexed.tokens.active()).tokens;
        let len = tokens.len();
        assert!(len > 10);
        let mut cursor = FilteredCursor::new(tokens.clone());
        for _ in 0..len + 2 {
            assert_eq!(cursor.current(), tokens.get(cursor.pos()).as_ref());
            cursor.advance();
        }
        cursor.advance_to_end();
        assert_eq!((cursor.pos(), cursor.current()), (len, None));
        assert_eq!(cursor.into_tokens().items().count(), len);
    }
}
