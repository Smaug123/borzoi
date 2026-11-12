//! The raw token table: the lossless full-trivia token stream of a source as a
//! struct of arrays, one [`RawKind`] and one `u32` start offset per token.
//!
//! The full-trivia stream tiles the source (every byte belongs to exactly one
//! token, trivia, directive line or inactive region, in order), so a token's
//! end is the next token's start and the table stores starts alone, with the
//! source length after the last. That is 6 bytes per token, against the 56 of
//! the driver's `(Result<TriviaToken, PreprocError>, Range<usize>)` item. Each
//! item is rebuilt on demand from its kind and its text ([`RawTokens::get`],
//! [`RawTokens::items`]); see `docs/token-pipeline-soa-plan.md` (D1, D2).
//!
//! The table is built from the full-trivia driver iterator, which stays the
//! reference implementation of the stream: under the `token-reference`
//! feature, [`lex_full_trivia`] asserts on every call that the table rebuilds
//! exactly the stream the iterator yielded.

use std::collections::HashSet;
use std::ops::{Bound, Range, RangeBounds};

use crate::directives::{PreprocError, TriviaToken, lex_with_symbols_full_trivia};
use crate::lexer::{LexError, Token, TokenKind};
use crate::syntax::SyntaxKind;

/// One item of the full-trivia stream, as the driver iterator yields it and as
/// [`RawTokens`] rebuilds it.
pub type RawItem<'a> = (Result<TriviaToken<'a>, PreprocError>, Range<usize>);

/// The kind of one entry of the full-trivia stream: a lexed token, a directive
/// or inactive-code trivia token, or a lexer error from active code.
///
/// The preprocessor's other errors are not entries: their spans do not tile
/// the source (a malformed directive's bytes are its trivia token's too), so
/// [`lex_full_trivia`] keeps them beside the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RawKind {
    /// A token from active code ([`TriviaToken::Lexed`]).
    Lexed(TokenKind),
    /// [`TriviaToken::HashLine`].
    HashLine,
    /// [`TriviaToken::WarnDirective`].
    WarnDirective,
    /// [`TriviaToken::HashIf`].
    HashIf,
    /// [`TriviaToken::HashElse`].
    HashElse,
    /// [`TriviaToken::HashElif`].
    HashElif,
    /// [`TriviaToken::HashEndif`].
    HashEndif,
    /// [`TriviaToken::InactiveCode`].
    InactiveCode,
    /// [`LexError::UnterminatedString`] in active code.
    UnterminatedString,
    /// [`LexError::UnterminatedComment`] in active code.
    UnterminatedComment,
    /// [`LexError::Unknown`] in active code.
    UnknownToken,
}

// One dataful variant, so the trivia and error kinds live in `TokenKind`'s
// niche and a kind costs what a `u16` does.
const _: () = assert!(std::mem::size_of::<RawKind>() == 2);

impl RawKind {
    /// The kinds that are not [`RawKind::Lexed`].
    pub const NOT_LEXED: &'static [RawKind] = &[
        RawKind::HashLine,
        RawKind::WarnDirective,
        RawKind::HashIf,
        RawKind::HashElse,
        RawKind::HashElif,
        RawKind::HashEndif,
        RawKind::InactiveCode,
        RawKind::UnterminatedString,
        RawKind::UnterminatedComment,
        RawKind::UnknownToken,
    ];

    /// The trivia [`SyntaxKind`] an entry of this kind is emitted as: active
    /// whitespace, a line break or a comment, or a directive or inactive-code
    /// marker. `None` for a significant token and for a lexer error.
    pub fn trivia_syntax_kind(self) -> Option<SyntaxKind> {
        match self {
            RawKind::Lexed(TokenKind::Whitespace) => Some(SyntaxKind::WHITESPACE),
            RawKind::Lexed(TokenKind::Newline) => Some(SyntaxKind::NEWLINE),
            RawKind::Lexed(TokenKind::LineComment) => Some(SyntaxKind::LINE_COMMENT),
            RawKind::Lexed(TokenKind::BlockComment) => Some(SyntaxKind::BLOCK_COMMENT),
            RawKind::Lexed(_) => None,
            RawKind::HashLine => Some(SyntaxKind::HASH_LINE),
            RawKind::WarnDirective => Some(SyntaxKind::WARN_DIRECTIVE),
            RawKind::HashIf => Some(SyntaxKind::HASH_IF),
            RawKind::HashElse => Some(SyntaxKind::HASH_ELSE),
            RawKind::HashElif => Some(SyntaxKind::HASH_ELIF),
            RawKind::HashEndif => Some(SyntaxKind::HASH_ENDIF),
            RawKind::InactiveCode => Some(SyntaxKind::INACTIVECODE),
            RawKind::UnterminatedString | RawKind::UnterminatedComment | RawKind::UnknownToken => {
                None
            }
        }
    }

    /// Whether an entry of this kind is trivia (see
    /// [`RawKind::trivia_syntax_kind`]).
    pub fn is_trivia(self) -> bool {
        self.trivia_syntax_kind().is_some()
    }

    /// The kind of a stream entry. `None` for the preprocessor errors that are
    /// not entries (see [`RawKind`]).
    fn of(res: &Result<TriviaToken<'_>, PreprocError>) -> Option<RawKind> {
        Some(match res {
            Ok(TriviaToken::Lexed(token)) => RawKind::Lexed(token.kind()),
            Ok(TriviaToken::HashLine) => RawKind::HashLine,
            Ok(TriviaToken::WarnDirective) => RawKind::WarnDirective,
            Ok(TriviaToken::HashIf) => RawKind::HashIf,
            Ok(TriviaToken::HashElse) => RawKind::HashElse,
            Ok(TriviaToken::HashElif) => RawKind::HashElif,
            Ok(TriviaToken::HashEndif) => RawKind::HashEndif,
            Ok(TriviaToken::InactiveCode) => RawKind::InactiveCode,
            Err(PreprocError::Lex(LexError::UnterminatedString)) => RawKind::UnterminatedString,
            Err(PreprocError::Lex(LexError::UnterminatedComment)) => RawKind::UnterminatedComment,
            Err(PreprocError::Lex(LexError::Unknown)) => RawKind::UnknownToken,
            Err(_) => return None,
        })
    }

    /// The stream entry of this kind whose text is `text`.
    pub(crate) fn rebuild(self, text: &str) -> Result<TriviaToken<'_>, PreprocError> {
        match self {
            RawKind::Lexed(kind) => Ok(TriviaToken::Lexed(Token::from_kind(kind, text))),
            RawKind::HashLine => Ok(TriviaToken::HashLine),
            RawKind::WarnDirective => Ok(TriviaToken::WarnDirective),
            RawKind::HashIf => Ok(TriviaToken::HashIf),
            RawKind::HashElse => Ok(TriviaToken::HashElse),
            RawKind::HashElif => Ok(TriviaToken::HashElif),
            RawKind::HashEndif => Ok(TriviaToken::HashEndif),
            RawKind::InactiveCode => Ok(TriviaToken::InactiveCode),
            RawKind::UnterminatedString => Err(PreprocError::Lex(LexError::UnterminatedString)),
            RawKind::UnterminatedComment => Err(PreprocError::Lex(LexError::UnterminatedComment)),
            RawKind::UnknownToken => Err(PreprocError::Lex(LexError::Unknown)),
        }
    }
}

/// The full-trivia token stream of one source. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct RawTokens<'src> {
    source: &'src str,
    kinds: Vec<RawKind>,
    /// `starts[i]` is token `i`'s first byte; `starts[len]` is the source
    /// length, so token `i` spans `starts[i]..starts[i + 1]`.
    starts: Vec<u32>,
}

impl<'src> RawTokens<'src> {
    /// The number of tokens.
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Whether the source has no tokens (it is empty).
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Token `i`'s kind.
    pub fn kind(&self, i: usize) -> RawKind {
        self.kinds[i]
    }

    /// Token `i`'s byte span.
    pub fn span(&self, i: usize) -> Range<usize> {
        self.starts[i] as usize..self.starts[i + 1] as usize
    }

    /// Token `i`'s text.
    pub fn text(&self, i: usize) -> &'src str {
        &self.source[self.span(i)]
    }

    /// Token `i`'s lexed token, or `None` if it is directive or inactive-code
    /// trivia or a lexer error.
    pub fn token(&self, i: usize) -> Option<Token<'src>> {
        match self.kinds[i] {
            RawKind::Lexed(kind) => Some(Token::from_kind(kind, self.text(i))),
            _ => None,
        }
    }

    /// Token `i` as the driver iterator yielded it, or `None` past the end.
    pub fn get(&self, i: usize) -> Option<RawItem<'src>> {
        (i < self.len()).then(|| self.item(i))
    }

    fn item(&self, i: usize) -> RawItem<'src> {
        let span = self.span(i);
        (self.kinds[i].rebuild(&self.source[span.clone()]), span)
    }

    /// The tokens with indices in `range`, as the driver iterator yielded them.
    pub fn items(
        &self,
        range: impl RangeBounds<usize>,
    ) -> impl DoubleEndedIterator<Item = RawItem<'src>> + ExactSizeIterator + '_ {
        let start = match range.start_bound() {
            Bound::Included(&i) => i,
            Bound::Excluded(&i) => i + 1,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&i) => i + 1,
            Bound::Excluded(&i) => i,
            Bound::Unbounded => self.len(),
        };
        assert!(
            start <= end && end <= self.len(),
            "token range {start}..{end} out of bounds for {} tokens",
            self.len()
        );
        (start..end).map(|i| self.item(i))
    }

    /// How many tokens start before `byte`: the index of the first token
    /// starting at or after it (`len()` if none does).
    pub fn count_starting_before(&self, byte: usize) -> usize {
        self.starts[..self.len()].partition_point(|&start| (start as usize) < byte)
    }

    /// How many tokens end at or before `byte`: the index of the first token
    /// ending after it (`len()` if none does).
    pub fn count_ending_by(&self, byte: usize) -> usize {
        self.starts[1..].partition_point(|&end| (end as usize) <= byte)
    }

    /// The active-code tokens and lexer errors: the stream the lex-filter and
    /// the productions consume. Directive and inactive-code trivia are not
    /// active code.
    pub fn active(
        &self,
    ) -> impl Iterator<Item = (Result<Token<'src>, LexError>, Range<usize>)> + '_ {
        self.kinds
            .iter()
            .zip(self.starts.windows(2))
            .filter_map(|(&kind, bounds)| {
                let span = bounds[0] as usize..bounds[1] as usize;
                let res = match kind {
                    RawKind::Lexed(kind) => Ok(Token::from_kind(kind, &self.source[span.clone()])),
                    RawKind::UnterminatedString => Err(LexError::UnterminatedString),
                    RawKind::UnterminatedComment => Err(LexError::UnterminatedComment),
                    RawKind::UnknownToken => Err(LexError::Unknown),
                    RawKind::HashLine
                    | RawKind::WarnDirective
                    | RawKind::HashIf
                    | RawKind::HashElse
                    | RawKind::HashElif
                    | RawKind::HashEndif
                    | RawKind::InactiveCode => return None,
                };
                Some((res, span))
            })
    }
}

/// A source lexed in full-trivia mode: the token table, and what the parse
/// needs from the preprocessor beside it.
#[derive(Debug, Clone)]
pub struct LexedSource<'src> {
    pub tokens: RawTokens<'src>,
    /// The preprocessor errors that are not stream entries (malformed or
    /// unbalanced directives, an `#if` unclosed at end of file), each with the
    /// span the driver paired it with, in stream order. See [`RawKind`].
    pub preproc_errors: Vec<(PreprocError, Range<usize>)>,
    /// The `#elif` directives FCS feature-checks; see
    /// [`crate::directives::FullTriviaDriver::elif_directives`].
    pub elif_directives: Vec<Range<usize>>,
}

/// Lex `source` in full-trivia mode under the conditional-compilation
/// `symbols`, in one pass over the full-trivia driver.
///
/// # Panics
///
/// If `source` is 4 GiB or longer (the token and syntax tables hold `u32`
/// offsets), or if the driver's stream does not tile the source, which the
/// lossless syntax tree depends on.
pub fn lex_full_trivia<'src>(source: &'src str, symbols: &HashSet<String>) -> LexedSource<'src> {
    let len = u32::try_from(source.len()).expect("a source is shorter than 4 GiB");
    let mut driver = lex_with_symbols_full_trivia(source, symbols);
    let mut kinds = Vec::new();
    let mut starts = Vec::new();
    let mut preproc_errors = Vec::new();
    #[cfg(feature = "token-reference")]
    let mut reference: Vec<RawItem<'src>> = Vec::new();
    let mut covered = 0usize;
    for (res, span) in driver.by_ref() {
        let Some(kind) = RawKind::of(&res) else {
            let Err(err) = res else {
                unreachable!("every token is an entry")
            };
            preproc_errors.push((err, span));
            continue;
        };
        assert_eq!(
            span.start, covered,
            "the full-trivia stream tiles the source: {kind:?} at {span:?}"
        );
        kinds.push(kind);
        // In range: `span.start` is at most the source length, checked above.
        starts.push(span.start as u32);
        covered = span.end;
        #[cfg(feature = "token-reference")]
        reference.push((res, span));
    }
    assert_eq!(
        covered,
        source.len(),
        "the full-trivia stream tiles the source"
    );
    starts.push(len);
    let tokens = RawTokens {
        source,
        kinds,
        starts,
    };
    #[cfg(feature = "token-reference")]
    {
        let rebuilt: Vec<RawItem<'src>> = tokens.items(..).collect();
        assert!(
            rebuilt == reference,
            "the token table does not rebuild the driver's stream for {source:?}"
        );
    }
    LexedSource {
        tokens,
        preproc_errors,
        elif_directives: driver.elif_directives().to_vec(),
    }
}
