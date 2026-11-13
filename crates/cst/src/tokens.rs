//! The token tables: the lossless full-trivia token stream of a source as a
//! struct of arrays, one [`RawKind`] and one `u32` start offset per token
//! ([`RawTokens`]), and the filtered stream the productions read
//! ([`FilteredTokens`]).
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
use crate::lexfilter::{FilteredItem, FilteredToken, Virtual};
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

/// The kind of one entry of the filtered stream: what the productions read.
///
/// The filter emits no trivia, so a [`FilteredKind::Token`] is never a trivia
/// kind ([`FilteredTokens::push`] asserts it). A `<` or `>` the filter promoted
/// to delimit a type application is a kind of its own, since the lexer's
/// [`Token::Less`] / [`Token::Greater`] carry `false` and the promotion is the
/// filter's fact, not the lexer's.
///
/// Its two dataful variants do not share a niche, so a kind is 4 bytes, not 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FilteredKind {
    /// A lexed token, passed through or split off a fused one by the filter.
    /// A [`TokenKind::Less`] / [`TokenKind::Greater`] is the unpromoted
    /// `Less(false)` / `Greater(false)`.
    Token(TokenKind),
    /// `Token::Less(true)`.
    TyparLess,
    /// `Token::Greater(true)`.
    TyparGreater,
    /// A token the filter synthesised.
    Virtual(Virtual),
    /// [`LexError::UnterminatedString`] from active code, passed through.
    UnterminatedString,
    /// [`LexError::UnterminatedComment`] from active code, passed through.
    UnterminatedComment,
    /// [`LexError::Unknown`] from active code, passed through.
    UnknownToken,
}

const _: () = assert!(std::mem::size_of::<FilteredKind>() == 4);

impl FilteredKind {
    /// The kind of a filtered-stream entry.
    ///
    /// # Panics
    ///
    /// On a trivia token, which the filter never emits.
    pub fn of(res: &Result<FilteredToken<'_>, LexError>) -> FilteredKind {
        match res {
            Ok(FilteredToken::Raw(Token::Less(true))) => FilteredKind::TyparLess,
            Ok(FilteredToken::Raw(Token::Greater(true))) => FilteredKind::TyparGreater,
            Ok(FilteredToken::Raw(token)) => {
                let kind = token.kind();
                assert!(
                    !RawKind::Lexed(kind).is_trivia(),
                    "the filtered stream holds no trivia, but has {token:?}"
                );
                FilteredKind::Token(kind)
            }
            Ok(FilteredToken::Virtual(v)) => FilteredKind::Virtual(*v),
            Err(LexError::UnterminatedString) => FilteredKind::UnterminatedString,
            Err(LexError::UnterminatedComment) => FilteredKind::UnterminatedComment,
            Err(LexError::Unknown) => FilteredKind::UnknownToken,
        }
    }

    /// Whether an entry of this kind is a lexer error.
    pub fn is_error(self) -> bool {
        matches!(
            self,
            FilteredKind::UnterminatedString
                | FilteredKind::UnterminatedComment
                | FilteredKind::UnknownToken
        )
    }

    /// The filtered-stream entry of this kind whose text is `text`.
    pub fn rebuild(self, text: &str) -> Result<FilteredToken<'_>, LexError> {
        self.rebuild_with(|| text)
    }

    /// [`FilteredKind::rebuild`], asking for the text only for a lexed token.
    /// Only a lexed token's span is a slice of the source: a virtual token's
    /// may start inside a multi-byte character (an `OBLOCKSEP` placed after a
    /// line's last token takes its span from that token's end).
    #[inline]
    pub fn rebuild_with<'a>(
        self,
        text: impl FnOnce() -> &'a str,
    ) -> Result<FilteredToken<'a>, LexError> {
        match self {
            FilteredKind::Token(kind) => Ok(FilteredToken::Raw(Token::from_kind(kind, text()))),
            FilteredKind::TyparLess => Ok(FilteredToken::Raw(Token::Less(true))),
            FilteredKind::TyparGreater => Ok(FilteredToken::Raw(Token::Greater(true))),
            FilteredKind::Virtual(v) => Ok(FilteredToken::Virtual(v)),
            FilteredKind::UnterminatedString => Err(LexError::UnterminatedString),
            FilteredKind::UnterminatedComment => Err(LexError::UnterminatedComment),
            FilteredKind::UnknownToken => Err(LexError::Unknown),
        }
    }
}

/// The filtered token stream of one source: the stream the productions read,
/// as a struct of arrays, one [`FilteredKind`] and one span per token.
///
/// Its spans do not tile the source (a virtual token is zero-width or borrows
/// its neighbour's span, a split `@>|}` yields two overlapping tokens, and a
/// sign fold merges two), so each token keeps both ends. As in [`RawTokens`],
/// a token's payload is its text: every entry the filter emits has
/// `source[span]` as its text, which [`FilteredTokens::push`] asserts in a
/// debug build, so [`FilteredTokens::get`] rebuilds the entry from its kind and
/// span. See `docs/token-pipeline-soa-plan.md` (D1, D2, D5).
#[derive(Debug, Clone)]
pub struct FilteredTokens<'src> {
    source: &'src str,
    kinds: Vec<FilteredKind>,
    starts: Vec<u32>,
    ends: Vec<u32>,
}

impl<'src> FilteredTokens<'src> {
    /// An empty table over `source`.
    ///
    /// # Panics
    ///
    /// If `source` is 4 GiB or longer (the table holds `u32` offsets).
    pub fn new(source: &'src str) -> Self {
        assert!(
            u32::try_from(source.len()).is_ok(),
            "a source is shorter than 4 GiB"
        );
        FilteredTokens {
            source,
            kinds: Vec::new(),
            starts: Vec::new(),
            ends: Vec::new(),
        }
    }

    /// The table of `items`, over `source`. Panics as [`FilteredTokens::new`]
    /// and [`FilteredTokens::push`] do.
    pub fn from_items(
        source: &'src str,
        items: impl IntoIterator<Item = FilteredItem<'src>>,
    ) -> Self {
        let mut table = FilteredTokens::new(source);
        for item in items {
            table.push(item);
        }
        table
    }

    /// Append an entry of the filtered stream.
    ///
    /// # Panics
    ///
    /// On a trivia token (see [`FilteredKind::of`]), or on a span outside the
    /// source; in a debug build, also on a token whose payload is not
    /// `source[span]`, which the table could not rebuild.
    pub fn push(&mut self, (res, span): FilteredItem<'src>) {
        let kind = FilteredKind::of(&res);
        debug_assert!(
            kind.rebuild_with(|| &self.source[span.clone()]) == res,
            "{res:?} at {span:?} is not rebuilt from its kind and text"
        );
        self.push_kind(kind, span);
    }

    fn push_kind(&mut self, kind: FilteredKind, span: Range<usize>) {
        assert!(
            span.start <= span.end && span.end <= self.source.len(),
            "{kind:?} at {span:?} lies outside a source of {} bytes",
            self.source.len()
        );
        self.kinds.push(kind);
        // In range: `span.end` is at most the source length, which `new`
        // bounds below 4 GiB.
        self.starts.push(span.start as u32);
        self.ends.push(span.end as u32);
    }

    /// The number of tokens.
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Whether the stream is empty.
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Token `i`'s kind.
    pub fn kind(&self, i: usize) -> FilteredKind {
        self.kinds[i]
    }

    /// Token `i`'s byte span.
    pub fn span(&self, i: usize) -> Range<usize> {
        self.starts[i] as usize..self.ends[i] as usize
    }

    /// Token `i`'s text, for a lexed token ([`FilteredKind::Token`]).
    ///
    /// # Panics
    ///
    /// On another kind whose span does not lie on character boundaries (see
    /// [`FilteredKind::rebuild_with`]).
    pub fn text(&self, i: usize) -> &'src str {
        &self.source[self.span(i)]
    }

    /// Token `i`'s kind, or `None` past the end. Cheaper than
    /// [`FilteredTokens::get`] for a lookahead that tests the kind alone.
    pub fn kind_at(&self, i: usize) -> Option<FilteredKind> {
        self.kinds.get(i).copied()
    }

    /// Token `i`'s span, or `None` past the end.
    pub fn span_at(&self, i: usize) -> Option<Range<usize>> {
        (i < self.len()).then(|| self.span(i))
    }

    /// Token `i` as the filter emitted it, or `None` past the end.
    #[inline]
    pub fn get(&self, i: usize) -> Option<FilteredItem<'src>> {
        (i < self.len()).then(|| self.item(i))
    }

    #[inline]
    fn item(&self, i: usize) -> FilteredItem<'src> {
        let span = self.span(i);
        (
            self.kinds[i].rebuild_with(|| &self.source[span.clone()]),
            span,
        )
    }

    /// The tokens from index `from` on, as the filter emitted them.
    pub fn items_from(
        &self,
        from: usize,
    ) -> impl DoubleEndedIterator<Item = FilteredItem<'src>> + ExactSizeIterator + '_ {
        (from.min(self.len())..self.len()).map(|i| self.item(i))
    }

    /// Every token, as the filter emitted it.
    pub fn items(
        &self,
    ) -> impl DoubleEndedIterator<Item = FilteredItem<'src>> + ExactSizeIterator + '_ {
        self.items_from(0)
    }

    /// Every token, as the filter emitted it, consuming the table.
    pub fn into_items(self) -> impl Iterator<Item = FilteredItem<'src>> {
        (0..self.len()).map(move |i| self.item(i))
    }

    /// Overwrite token `i` with a token of kind `kind` spanning `span`.
    pub(crate) fn set(&mut self, i: usize, kind: FilteredKind, span: Range<usize>) {
        assert!(
            span.start <= span.end && span.end <= self.source.len(),
            "{kind:?} at {span:?} lies outside a source of {} bytes",
            self.source.len()
        );
        self.kinds[i] = kind;
        self.starts[i] = span.start as u32;
        self.ends[i] = span.end as u32;
    }

    /// Copy token `from` over token `to`.
    pub(crate) fn copy_within(&mut self, from: usize, to: usize) {
        self.kinds[to] = self.kinds[from];
        self.starts[to] = self.starts[from];
        self.ends[to] = self.ends[from];
    }

    /// Keep the first `len` tokens.
    pub(crate) fn truncate(&mut self, len: usize) {
        self.kinds.truncate(len);
        self.starts.truncate(len);
        self.ends.truncate(len);
    }
}
