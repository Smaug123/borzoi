//! Stateful preprocessor driver: `#if` / `#elif` / `#else` / `#endif`.
//!
//! Wraps the Logos lexer with a small state machine that tracks the
//! `#if`-stack and only drives the lexer in active branches. Inactive
//! bodies are byte-walked without being tokenised, which is what lets us
//! handle fixtures like `ConditionalCompilation/InComment01.fs` whose
//! inactive arm contains an unterminated `(*` — the raw lexer would
//! choke, but the directive layer never feeds those bytes to it.
//!
//! Public API:
//!
//! ```ignore
//! pub fn lex_with_symbols<'a, 'b>(
//!     source: &'a str,
//!     symbols: &'b HashSet<String>,
//! ) -> Driver<'a, 'b>
//! ```
//!
//! The two lifetimes are deliberately separate: emitted tokens borrow
//! only from `source` (their `Item` is `(Result<Token<'a>, _>, _)`), so a
//! caller may build a short-lived `HashSet` of symbols just for
//! preprocessing and still hold on to the collected tokens for as long
//! as `source` lives.
//!
//! The returned [`Driver`] is `Iterator<Item = (Result<Token<'a>,
//! PreprocError>, Range<usize>)>`. Directive errors and preprocessor
//! semantic errors (unmatched `#endif`, double `#else`, unclosed `#if`
//! at EOF, …) appear inline in the stream as `Err(_)` items, the same
//! way Logos surfaces lexer errors. The directive layer is recoverable:
//! a malformed directive still drives the state machine (a bad `#if`
//! body is treated as `false`, an unmatched `#endif` is reported and
//! then ignored), so a single error doesn't desynchronise the rest of
//! the file.
//!
//! Correctness oracle: `reference_lex` (test-only) builds an
//! "active-byte mask" by walking the source line by line, lexes the
//! original source, and drops tokens whose span is fully inside an
//! inactive byte range. The fast driver and the reference implementation
//! must agree on every input that contains no multi-line tokens (strings
//! or block comments), per the property test below.

use std::collections::HashSet;
use std::collections::VecDeque;
use std::ops::Range;

use crate::directives::expr::Expr;
use crate::directives::line::{
    Directive, DirectiveError, DirectiveErrorKind, DirectiveKind, Recognised, recognise_directive,
};
use crate::directives::line_store::{LineDirective, LineDirectiveStore, line_index};
use crate::lexer::interp::{Frame as InterpFrame, InterpDriver};
use crate::lexer::{LexError, Token};
use crate::syntax::SyntaxKind;

/// Errors reported by the preprocessor layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreprocError {
    /// Underlying Logos lexer error from an active branch.
    Lex(LexError),
    /// A directive line was malformed (`#if !`, `#endif foo`, …). The
    /// driver still updates state: `#if`/`#elif` with a bad body are
    /// treated as `false`; `#else`/`#endif` with trailing garbage still
    /// pop / flip the stack.
    Directive(DirectiveError),
    /// `#endif` with no matching `#if`.
    UnmatchedEndIf { range: Range<usize> },
    /// `#else` with an empty stack.
    OrphanElse { range: Range<usize> },
    /// `#elif` with an empty stack.
    OrphanElif { range: Range<usize> },
    /// `#else` after a prior `#else` in the same chain.
    DoubleElse {
        range: Range<usize>,
        prev_else: Range<usize>,
    },
    /// `#elif` after `#else` in the same chain.
    ElifAfterElse {
        range: Range<usize>,
        prev_else: Range<usize>,
    },
    /// EOF reached with at least one `#if` frame unclosed.
    UnclosedIfAtEof { if_span: Range<usize>, eof: usize },
}

impl PreprocError {
    /// The byte span this error should be squiggled at, given the span the
    /// driver paired with it in its token stream. For every variant this is
    /// the paired span, except [`UnclosedIfAtEof`](PreprocError::UnclosedIfAtEof),
    /// whose paired span is a zero-width point at EOF — there we use the opening
    /// `#if`'s span so the diagnostic lands on visible source.
    ///
    /// This is the single source of truth for "which bytes does a preproc error
    /// cover", shared by the parser (which surfaces these in its error list) and
    /// the LSP's lexer-diagnostics producer. Sharing it keeps the two in lockstep
    /// so the LSP's overlap-dedup reliably drops the parser's duplicate of an
    /// error the lexer producer already reports.
    pub fn reporting_span(&self, paired_span: Range<usize>) -> Range<usize> {
        match self {
            PreprocError::UnclosedIfAtEof { if_span, .. } => if_span.clone(),
            _ => paired_span,
        }
    }

    /// A human-readable, one-line diagnostic message for this error. Used by
    /// the parser when it surfaces a preprocessor error in its error list and
    /// by the LSP's lexer-diagnostics producer, so the two never drift.
    pub fn diagnostic_message(&self) -> String {
        match self {
            PreprocError::Lex(e) => lex_error_message(e).to_string(),
            PreprocError::Directive(e) => directive_error_message(e),
            PreprocError::UnmatchedEndIf { .. } => "`#endif` with no matching `#if`".to_string(),
            PreprocError::OrphanElse { .. } => "`#else` with no matching `#if`".to_string(),
            PreprocError::OrphanElif { .. } => "`#elif` with no matching `#if`".to_string(),
            PreprocError::DoubleElse { .. } => {
                "`#else` after a previous `#else` in the same `#if` chain".to_string()
            }
            PreprocError::ElifAfterElse { .. } => {
                "`#elif` after `#else` in the same `#if` chain".to_string()
            }
            PreprocError::UnclosedIfAtEof { .. } => "`#if` without matching `#endif`".to_string(),
        }
    }
}

/// One-line message for a raw lexer error surfaced from an active branch.
fn lex_error_message(err: &LexError) -> &'static str {
    match err {
        LexError::UnterminatedString => "unterminated string literal",
        LexError::UnterminatedComment => "unterminated block comment",
        LexError::Unknown => "unrecognised token",
    }
}

/// One-line message for a malformed directive line.
fn directive_error_message(err: &DirectiveError) -> String {
    match &err.kind {
        DirectiveErrorKind::MissingSeparator => {
            "malformed directive: whitespace is required before the condition".to_string()
        }
        DirectiveErrorKind::MissingExpression => {
            "malformed directive: missing condition".to_string()
        }
        DirectiveErrorKind::ExpressionParse(e) => format!("malformed directive condition: {e}"),
        DirectiveErrorKind::UnexpectedTokensAfterDirective => {
            "malformed directive: unexpected tokens after the directive".to_string()
        }
    }
}

/// A token in the shared preprocessor stream: either a real lexer [`Token`]
/// from an active branch, or a trivia marker for a directive line or a dead
/// (`#if`-eliminated) region that swallow-mode (FCS `skip=true`) drops.
/// Mirrors FCS's `skip=false` behaviour, where these surface as the
/// `HASH_LINE` / `WARN_DIRECTIVE` / `HASH_IF` / … / `INACTIVECODE` hidden
/// tokens.
///
/// The public [`Driver`] only ever yields [`TriviaToken::Lexed`] (it runs
/// the core in swallow mode); the marker variants are produced only by the
/// full-trivia mode. The `#line` / `#nowarn` / `#warnon` markers
/// ([`HashLine`](TriviaToken::HashLine) /
/// [`WarnDirective`](TriviaToken::WarnDirective)) are emitted by the trivia
/// driver; the conditional-compilation markers
/// ([`HashIf`](TriviaToken::HashIf) …
/// [`InactiveCode`](TriviaToken::InactiveCode)) by a later stage — see
/// `docs/completed/parser-ifdef-plan.md`.
#[derive(Debug, Clone, PartialEq)]
pub enum TriviaToken<'a> {
    /// A real lexer token from an active branch.
    Lexed(Token<'a>),
    /// `#line N` / `# N "file"` — maps to
    /// [`crate::syntax::SyntaxKind::HASH_LINE`].
    HashLine,
    /// `#nowarn …` / `#warnon …` — maps to
    /// [`crate::syntax::SyntaxKind::WARN_DIRECTIVE`].
    WarnDirective,
    /// `#if …` — maps to [`crate::syntax::SyntaxKind::HASH_IF`].
    HashIf,
    /// `#else` — maps to [`crate::syntax::SyntaxKind::HASH_ELSE`].
    HashElse,
    /// `#elif …` — maps to [`crate::syntax::SyntaxKind::HASH_ELIF`].
    HashElif,
    /// `#endif` — maps to [`crate::syntax::SyntaxKind::HASH_ENDIF`].
    HashEndif,
    /// A dead (`#if`-eliminated) region — maps to
    /// [`crate::syntax::SyntaxKind::INACTIVECODE`].
    InactiveCode,
}

impl<'a> TriviaToken<'a> {
    /// The trivia [`SyntaxKind`] this marker maps to, or `None` for
    /// [`TriviaToken::Lexed`] (whose tree kind is the parser's concern).
    /// This is the single edge connecting the full-trivia stream to the
    /// directive / inactive-code tree vocabulary.
    pub fn trivia_syntax_kind(&self) -> Option<SyntaxKind> {
        match self {
            TriviaToken::HashLine => Some(SyntaxKind::HASH_LINE),
            TriviaToken::WarnDirective => Some(SyntaxKind::WARN_DIRECTIVE),
            TriviaToken::HashIf => Some(SyntaxKind::HASH_IF),
            TriviaToken::HashElse => Some(SyntaxKind::HASH_ELSE),
            TriviaToken::HashElif => Some(SyntaxKind::HASH_ELIF),
            TriviaToken::HashEndif => Some(SyntaxKind::HASH_ENDIF),
            TriviaToken::InactiveCode => Some(SyntaxKind::INACTIVECODE),
            TriviaToken::Lexed(_) => None,
        }
    }
}

/// The trivia-token marker for a recognised trivia directive. The
/// conditional-compilation directives never reach here — `is_trivia()`
/// gates the only caller — so they map to `None`.
fn directive_trivia_token<'a>(directive: &Directive) -> Option<TriviaToken<'a>> {
    match directive {
        Directive::Line { .. } => Some(TriviaToken::HashLine),
        Directive::NoWarn { .. } | Directive::WarnOn { .. } => Some(TriviaToken::WarnDirective),
        Directive::If(_) | Directive::Elif(_) | Directive::Else | Directive::EndIf => None,
    }
}

/// The conditional-compilation trivia marker for a CC directive keyword.
fn cc_directive_marker<'a>(kind: DirectiveKind) -> TriviaToken<'a> {
    match kind {
        DirectiveKind::If => TriviaToken::HashIf,
        DirectiveKind::Elif => TriviaToken::HashElif,
        DirectiveKind::Else => TriviaToken::HashElse,
        DirectiveKind::EndIf => TriviaToken::HashEndif,
    }
}

/// One entry on the `#if`-stack — the state of a single open `#if`-chain.
#[derive(Debug, Clone)]
struct Frame {
    /// Span of the `#if` directive that opened this frame.
    if_span: Range<usize>,
    /// Is the current arm (the most recent of `#if` / `#elif` / `#else`)
    /// lit? "Lit" means: this arm's expression evaluated to true *and*
    /// no prior arm in the chain was lit. Whether the driver is in
    /// active mode is the conjunction over all frames' `arm_lit`.
    arm_lit: bool,
    /// Has any arm in this chain ever been lit? Once true, no further
    /// `#elif` / `#else` arm may light up — this mirrors FCS where a
    /// chain has at most one selected arm.
    any_arm_lit: bool,
    /// Span of the `#else` directive in this chain, if any. Set means
    /// subsequent `#elif`/`#else` are diagnostic errors.
    else_span: Option<Range<usize>>,
}

/// Public swallow-mode preprocessor iterator (FCS `skip=true`): yields the
/// active-branch [`Token`] stream with `#line` / `#nowarn` / `#warnon`
/// directives swallowed, exactly as before this refactor. A thin wrapper
/// over the private `DriverCore`; in swallow mode the core never yields a
/// directive marker, so unwrapping [`TriviaToken::Lexed`] in `next` is
/// total.
pub struct Driver<'a, 'b>(DriverCore<'a, 'b>);

impl<'a, 'b> Iterator for Driver<'a, 'b> {
    type Item = DriverItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(res, span)| {
            let res = res.map(|tt| match tt {
                TriviaToken::Lexed(tok) => tok,
                // Swallow mode never emits a directive / inactive-code marker.
                _ => unreachable!("DriverCore in swallow mode never yields directive trivia"),
            });
            (res, span)
        })
    }
}

impl<'a, 'b> Driver<'a, 'b> {
    /// The active-branch `#line` directives seen so far, in source order.
    /// Delegates to the inner core's `line_directives`.
    pub fn line_directives(&self) -> &LineDirectiveStore {
        self.0.line_directives()
    }
}

/// Shared preprocessor state machine. See module docs. Produces a
/// [`CoreItem`] stream whose success payload is a [`TriviaToken`]; the
/// public [`Driver`] wraps it and unwraps [`TriviaToken::Lexed`]. Factored
/// out so a later full-trivia mode can reuse the identical state machine
/// while exposing the directive markers it currently swallows.
struct DriverCore<'a, 'b> {
    source: &'a str,
    symbols: &'b HashSet<String>,
    /// Next byte to consider. Either at a line start (we may try to
    /// recognise a directive) or mid-token (we drive Logos).
    pos: usize,
    stack: Vec<Frame>,
    /// Items the driver has produced internally but not yet yielded —
    /// chiefly errors that need to come out of a single `apply_directive`
    /// call. Drained first on every `next`.
    pending: VecDeque<CoreItem<'a>>,
    /// One-shot flag: have we emitted the EOF unclosed-if errors yet?
    eof_done: bool,
    /// Logos lexer over `source[lexer_state.base..]`, lazily (re)created.
    /// We invalidate it whenever `pos` jumps non-contiguously (e.g. after
    /// processing a directive).
    lexer: Option<LexerState<'a>>,
    /// Interp-frame stack preserved across `invalidate_lexer()` calls.
    /// Snapshotted from the inner [`InterpDriver`] when invalidation
    /// happens mid-fill (e.g. `#if`/`#endif` inside a `$"{ … }"`) and
    /// fed back to the next [`InterpDriver`] on recreation, so a fill
    /// that straddles directive boundaries still recognises its closing
    /// `}` rather than mis-tokenising it as a stray `RBrace`. Empty
    /// when no fill is active.
    pending_interp_frames: Vec<InterpFrame>,
    /// `#line` directives seen in active branches, in source order. Built
    /// up as a side effect of scanning; consumed by a later stage that
    /// remaps diagnostic spans (see `docs/completed/line-directive-remap-plan.md`).
    line_directives: LineDirectiveStore,
    /// Byte offset up to which `line_scan_line` has counted line breaks.
    /// Advances monotonically as `#line` directives are captured (they are
    /// recognised in source order), so the running line count is computed
    /// once over each stretch of source rather than rescanning from offset
    /// `0` per directive — keeping capture linear in the source length.
    line_scan_offset: usize,
    /// 0-based line index of `source[..line_scan_offset]`.
    line_scan_line: u32,
    /// Full-trivia mode (FCS `skip=false`). When set, active-branch trivia
    /// directives (`#line` / `#nowarn` / `#warnon`) are emitted as
    /// [`TriviaToken`] markers over their recognised range instead of being
    /// swallowed. `false` for the public [`Driver`]; `true` for
    /// [`FullTriviaDriver`].
    trivia_mode: bool,
    /// Full-trivia gap-fill cursor: the end of the last emitted token. Any
    /// bytes between this and the next emitted token are a dead
    /// (`#if`-eliminated) region, surfaced as one `INACTIVECODE` token so the
    /// emitted spans tile the source. Unused in swallow mode.
    covered_end: usize,
    /// Spans of the `#elif` directives FCS feature-checks for
    /// `LanguageFeature.PreprocessorElif` — every `#elif` with a separating
    /// whitespace body (`anywhite+ anystring`) at line start, in active *and*
    /// skipped branches and at any nesting depth (lex.fsl's three
    /// `CheckLanguageFeatureAndRecover` sites). A bare `#elif` (no separator) is
    /// excluded, matching FCS, which treats it as whitespace. Recorded
    /// independent of language version; the parser turns each into an FS3350
    /// diagnostic when the pinned version predates 11.0. In source order.
    elif_directives: Vec<Range<usize>>,
}

type DriverItem<'a> = (Result<Token<'a>, PreprocError>, Range<usize>);
/// Like [`DriverItem`], but the success payload is a [`TriviaToken`] so the
/// shared [`DriverCore`] (and the future full-trivia mode) can carry
/// directive markers the public [`Driver`] never surfaces. All error items
/// are payload-agnostic, so they flow through both aliases unchanged.
type CoreItem<'a> = (Result<TriviaToken<'a>, PreprocError>, Range<usize>);

struct LexerState<'a> {
    /// Absolute byte offset where the inner [`InterpDriver`] was constructed.
    /// Add to each relative span emitted by `iter` to get an absolute span.
    base: usize,
    /// Absolute byte position where the *next* token would begin,
    /// assuming we have only driven the lexer forward since `base`. If
    /// `pos` differs from this, the lexer is stale.
    next_pos: usize,
    /// Interp-aware wrapper around the raw Logos lexer. Active interp-string
    /// frames live inside this driver; they are snapshotted into
    /// [`Driver::pending_interp_frames`] when the directive layer
    /// invalidates the lexer, then restored to the next `InterpDriver` on
    /// recreation so a fill straddling `#if`/`#endif` keeps its frame
    /// stack intact.
    iter: InterpDriver<'a>,
}

/// Construct a swallow-mode driver iterator over `source` with the given
/// symbol set (FCS `skip=true`). `#line` / `#nowarn` / `#warnon` directives
/// are recognised and dropped without emitting a token.
pub fn lex_with_symbols<'a, 'b>(source: &'a str, symbols: &'b HashSet<String>) -> Driver<'a, 'b> {
    Driver(DriverCore::new(source, symbols, false))
}

/// Construct a *full-trivia* driver iterator over `source` (FCS
/// `skip=false`): like [`lex_with_symbols`], but `#line` / `#nowarn` /
/// `#warnon` directives in **active** branches surface as
/// [`TriviaToken::HashLine`] / [`TriviaToken::WarnDirective`] tokens over
/// their recognised line range instead of being swallowed. Directives in
/// inactive (`#if`-eliminated) branches are still dropped — the compiler
/// never sees them. See `docs/completed/hashline-warndirective-trivia-plan.md`.
pub fn lex_with_symbols_full_trivia<'a, 'b>(
    source: &'a str,
    symbols: &'b HashSet<String>,
) -> FullTriviaDriver<'a, 'b> {
    FullTriviaDriver(DriverCore::new(source, symbols, true))
}

/// Full-trivia preprocessor iterator (FCS `skip=false`). Unlike [`Driver`],
/// its item is a [`TriviaToken`], so active-branch directive trivia is
/// preserved. Built by [`lex_with_symbols_full_trivia`].
pub struct FullTriviaDriver<'a, 'b>(DriverCore<'a, 'b>);

impl<'a, 'b> Iterator for FullTriviaDriver<'a, 'b> {
    type Item = (Result<TriviaToken<'a>, PreprocError>, Range<usize>);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

impl<'a, 'b> FullTriviaDriver<'a, 'b> {
    /// The active-branch `#line` directives seen so far, in source order.
    /// Delegates to the inner core's `line_directives`.
    pub fn line_directives(&self) -> &LineDirectiveStore {
        self.0.line_directives()
    }

    /// Spans of the `#elif` directives FCS feature-checks for
    /// `LanguageFeature.PreprocessorElif`, in source order — every `#elif` with
    /// a separating whitespace body, in any branch and at any nesting depth;
    /// bare `#elif` excluded. The parser maps each to an FS3350 diagnostic under
    /// a pre-11.0 language version. Valid once the driver has been drained.
    pub fn elif_directives(&self) -> &[Range<usize>] {
        &self.0.elif_directives
    }
}

impl<'a, 'b> Iterator for DriverCore<'a, 'b> {
    type Item = CoreItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }

            // At a line start, see if a directive begins here. This check
            // runs in *both* modes: in active mode we intercept the line
            // so Logos doesn't tokenise `#if`/`#endif` as `Hash`+`Ident`;
            // in skip mode this is how we discover the closer.
            if self.pos < self.source.len()
                && self.at_line_start()
                && let Some(result) = recognise_directive(self.source, self.pos)
            {
                self.handle_directive_result(result);
                continue;
            }

            if self.pos >= self.source.len() {
                if !self.eof_done {
                    self.eof_done = true;
                    let eof = self.source.len();
                    // Full-trivia: cover a trailing dead region (a file ending
                    // mid-dead-branch) before the unclosed-`#if` errors.
                    self.fill_inactive_gap(eof);
                    for frame in self.stack.drain(..) {
                        let if_span = frame.if_span.clone();
                        self.pending.push_back((
                            Err(PreprocError::UnclosedIfAtEof { if_span, eof }),
                            eof..eof,
                        ));
                    }
                    continue;
                }
                return None;
            }

            if self.currently_active() {
                let (tok, span) = self.next_logos_token()?;
                let (start, end) = (span.start, span.end);
                self.pos = end;
                let item = (tok.map(TriviaToken::Lexed).map_err(PreprocError::Lex), span);
                if self.trivia_mode {
                    // Close any preceding dead region, then queue the token so
                    // it interleaves with the markers in source order.
                    self.fill_inactive_gap(start);
                    self.covered_end = self.covered_end.max(end);
                    self.pending.push_back(item);
                    continue;
                }
                return Some(item);
            } else {
                self.advance_to_next_line_start();
                self.invalidate_lexer();
            }
        }
    }
}

impl<'a, 'b> DriverCore<'a, 'b> {
    fn new(source: &'a str, symbols: &'b HashSet<String>, trivia_mode: bool) -> Self {
        DriverCore {
            source,
            symbols,
            pos: 0,
            stack: Vec::new(),
            pending: VecDeque::new(),
            eof_done: false,
            lexer: None,
            pending_interp_frames: Vec::new(),
            line_directives: LineDirectiveStore::new(),
            line_scan_offset: 0,
            line_scan_line: 0,
            trivia_mode,
            covered_end: 0,
            elif_directives: Vec::new(),
        }
    }

    /// Full-trivia mode: push a [`TriviaToken::InactiveCode`] token covering
    /// `self.covered_end .. up_to` if that range is non-empty, then advance
    /// the cursor. No-op in swallow mode. Called before emitting any token so
    /// that a dead `#if`-eliminated region (including non-visible nested
    /// directives inside it) coalesces into one `INACTIVECODE` token and the
    /// emitted spans stay a gapless partition of the source.
    fn fill_inactive_gap(&mut self, up_to: usize) {
        if self.trivia_mode && up_to > self.covered_end {
            self.pending
                .push_back((Ok(TriviaToken::InactiveCode), self.covered_end..up_to));
            self.covered_end = up_to;
        }
    }

    /// Full-trivia mode: emit `marker` over `range`, first filling any dead
    /// gap before it (see [`Self::fill_inactive_gap`]).
    fn emit_trivia_marker(&mut self, marker: TriviaToken<'a>, range: Range<usize>) {
        self.fill_inactive_gap(range.start);
        self.covered_end = self.covered_end.max(range.end);
        self.pending.push_back((Ok(marker), range));
    }

    fn at_line_start(&self) -> bool {
        if self.pos == 0 {
            return true;
        }
        if self.pos > self.source.len() {
            return false;
        }
        let prev = self.source.as_bytes()[self.pos - 1];
        prev == b'\n' || prev == b'\r'
    }

    fn currently_active(&self) -> bool {
        self.stack.iter().all(|f| f.arm_lit)
    }

    /// The `#line` directives seen so far in active branches, in source
    /// order. Complete only once the iterator has been fully consumed;
    /// reading it mid-stream reflects directives encountered up to the last
    /// yielded token. See `docs/completed/line-directive-remap-plan.md`.
    pub fn line_directives(&self) -> &LineDirectiveStore {
        &self.line_directives
    }

    /// 0-based generated line of `offset`, advancing the cached scan
    /// position. `offset` must be `>= self.line_scan_offset` (callers
    /// capture directives in source order) and must sit on a line start, so
    /// the counted stretch never splits a `\r\n` pair. Counting only the new
    /// stretch keeps total capture work linear in the source length.
    fn generated_line_at(&mut self, offset: usize) -> u32 {
        let offset = offset.min(self.source.len());
        debug_assert!(offset >= self.line_scan_offset);
        let chunk = &self.source[self.line_scan_offset..offset];
        self.line_scan_line += line_index(chunk, chunk.len());
        self.line_scan_offset = offset;
        self.line_scan_line
    }

    fn handle_directive_result(&mut self, result: Result<Recognised, DirectiveError>) {
        // Trivia directives (`#nowarn` / `#warnon` / `#line`) never touch the
        // ifdef stack. In swallow mode (FCS `skip=true`) the driver advances
        // past the line without emitting a token; in full-trivia mode
        // (`skip=false`) an active-branch directive surfaces as a
        // `TriviaToken` over its line range. The recogniser doesn't produce
        // errors for trivia directives, so we only handle the `Ok` case here.
        if let Ok(r) = &result
            && r.directive.is_trivia()
        {
            // `currently_active()` reflects the branch the directive sits in
            // (the directive doesn't touch the ifdef stack). Dead-branch
            // directives are dropped in both modes: the F# compiler never
            // sees them, so they must not take effect or surface as trivia.
            if self.currently_active() {
                // Record active-branch `#line` directives for later span
                // remapping.
                if let Directive::Line { number, file } = &r.directive {
                    let generated_line = self.generated_line_at(r.range.start);
                    self.line_directives.push(LineDirective {
                        generated_line,
                        virtual_line: *number,
                        file: file.clone(),
                    });
                }
                // Full-trivia mode: surface the directive as a single trivia
                // token over its recognised range (gap-filling any dead
                // region before it). Swallow mode leaves `pending` untouched.
                if self.trivia_mode
                    && let Some(tt) = directive_trivia_token(&r.directive)
                {
                    self.emit_trivia_marker(tt, r.range.clone());
                }
            }
            self.pos = r.range.end;
            self.invalidate_lexer();
            return;
        }

        let (range, kind, expr_value, body_err, suppress_state) = match result {
            Ok(r) => {
                // Trivia case handled above; `kind()` returns `Some` for the
                // four CC variants that remain.
                let kind = r.directive.kind().expect("trivia handled above");
                let expr_value = match r.directive {
                    Directive::If(e) | Directive::Elif(e) => Some(self.eval_expr(&e)),
                    Directive::Else | Directive::EndIf => None,
                    Directive::NoWarn { .. }
                    | Directive::WarnOn { .. }
                    | Directive::Line { .. } => {
                        unreachable!("trivia handled above")
                    }
                };
                (r.range, kind, expr_value, None, false)
            }
            Err(e) => {
                let kind = e.keyword;
                let range = e.range.clone();
                let expr_value = match kind {
                    DirectiveKind::If | DirectiveKind::Elif => Some(false),
                    DirectiveKind::Else | DirectiveKind::EndIf => None,
                };
                // FCS has two distinct lex rules for `#if` / `#elif`:
                // - With separating whitespace (`#if anywhite+ anystring`):
                //   open a frame; if the body fails to parse, treat it as
                //   `false`. This covers `#if   `, `#if //c`, `#if !`,
                //   `#if FOO BAR` — all `MissingExpression` /
                //   `ExpressionParse` in our scheme.
                // - Without (bare `#if`, `#if(FOO)`): emit a diagnostic
                //   but do *not* open a frame; lexing continues in the
                //   surrounding mode. This is our `MissingSeparator`.
                // Mirror that split: suppress state changes only for the
                // no-separator case.
                let suppress_state = matches!(e.kind, DirectiveErrorKind::MissingSeparator)
                    && matches!(kind, DirectiveKind::If | DirectiveKind::Elif);
                (range, kind, expr_value, Some(e), suppress_state)
            }
        };

        // Language-version gate input. FCS runs `CheckLanguageFeatureAndRecover
        // PreprocessorElif` for every `#elif` that has a separating-whitespace
        // body — exactly the `!suppress_state` Elif case here (the body may
        // still fail to parse, e.g. `#elif !`; FCS feature-checks before
        // evaluating it) — at line start, in active and skipped branches and at
        // any nesting depth. Record the span unconditionally and ahead of the
        // parent-active gating below; the parser decides, from the pinned
        // language version, whether it is an error. A bare `#elif`
        // (`suppress_state`) is not feature-checked — FCS treats it as
        // whitespace, so it is excluded here too.
        if kind == DirectiveKind::Elif && !suppress_state {
            self.elif_directives.push(range.clone());
        }

        // Parent-active gating: FCS depth-skips directives nested under an
        // inactive `#if`, so chain diagnostics (`DoubleElse`,
        // `ElifAfterElse`) and body parse errors must not fire inside such
        // a context. Orphan / EOF errors are emitted unconditionally —
        // they signal a structural problem that's still useful to report.
        let parent_active = parent_active_for(&self.stack, kind);

        // Full-trivia mode: emit the directive line as its CC-directive
        // trivia token when it is *visible* (its enclosing context is
        // active). A directive nested inside a dead branch (`!parent_active`)
        // is absorbed into the surrounding INACTIVECODE instead, so it is not
        // emitted here — `fill_inactive_gap` covers its bytes.
        if self.trivia_mode && parent_active {
            self.emit_trivia_marker(cc_directive_marker(kind), range.clone());
        }

        if let Some(e) = body_err
            && parent_active
        {
            let r = e.range.clone();
            self.pending.push_back((Err(PreprocError::Directive(e)), r));
        }

        if !suppress_state {
            apply_directive(
                &mut self.stack,
                &mut self.pending,
                range.clone(),
                kind,
                expr_value,
                parent_active,
            );
        }
        self.pos = range.end;
        self.invalidate_lexer();
    }

    fn eval_expr(&self, e: &Expr) -> bool {
        e.eval(|name| self.symbols.contains(name))
    }

    fn advance_to_next_line_start(&mut self) {
        let bytes = self.source.as_bytes();
        while self.pos < bytes.len() {
            let b = bytes[self.pos];
            self.pos += 1;
            if b == b'\n' {
                return;
            }
            if b == b'\r' {
                if self.pos < bytes.len() && bytes[self.pos] == b'\n' {
                    self.pos += 1;
                }
                return;
            }
        }
    }

    fn next_logos_token(&mut self) -> Option<(Result<Token<'a>, LexError>, Range<usize>)> {
        let recreate = match &self.lexer {
            None => true,
            Some(state) => state.next_pos != self.pos,
        };
        if recreate {
            // Hand the preserved interp-frame stack (if any) to the new
            // driver so a fill that straddled the directive boundary
            // keeps its `}` recognition. Drained on use — once the next
            // `}` closes the frame, we don't want a later recreation to
            // resurrect it.
            let frames = std::mem::take(&mut self.pending_interp_frames);
            let iter = InterpDriver::new_with_frames(&self.source[self.pos..], frames);
            self.lexer = Some(LexerState {
                base: self.pos,
                next_pos: self.pos,
                iter,
            });
        }
        let state = self.lexer.as_mut()?;
        let (tok, rel_span) = state.iter.next()?;
        let abs_span = (rel_span.start + state.base)..(rel_span.end + state.base);
        state.next_pos = abs_span.end;
        Some((tok, abs_span))
    }

    fn invalidate_lexer(&mut self) {
        // Preserve any active interp-string frames across the
        // invalidation. The driver may be invalidated either because a
        // directive consumed the line (active or skipped) or because
        // we're scanning forward in skip mode to the next directive —
        // in both cases the fill it sits inside must keep its frame
        // stack so the eventual `}` lex-resolves correctly.
        if let Some(state) = self.lexer.as_ref() {
            self.pending_interp_frames = state.iter.snapshot_frames();
        }
        self.lexer = None;
    }
}

/// Is the *enclosing* context (ignoring the frame this directive will
/// modify) active? `#if` opens a new frame, so its parent is the full
/// stack; `#elif`/`#else`/`#endif` act on the top frame, so the parent
/// is everything below it. Empty stack ⇒ no parent ⇒ treated as active.
fn parent_active_for(stack: &[Frame], kind: DirectiveKind) -> bool {
    match kind {
        DirectiveKind::If => stack.iter().all(|f| f.arm_lit),
        DirectiveKind::Elif | DirectiveKind::Else | DirectiveKind::EndIf => {
            if stack.is_empty() {
                true
            } else {
                stack[..stack.len() - 1].iter().all(|f| f.arm_lit)
            }
        }
    }
}

/// Apply a recognised directive (well-formed or recovered) to the stack.
/// Free function so the reference implementation can reuse it.
///
/// `parent_active` (computed by [`parent_active_for`] before invocation)
/// controls whether chain diagnostics — `DoubleElse`, `ElifAfterElse` —
/// fire. Inside an inactive parent these are depth-skipped, matching FCS.
/// Orphan and unmatched-endif errors are emitted regardless: they signal
/// structural problems independent of containment.
fn apply_directive<T>(
    stack: &mut Vec<Frame>,
    pending: &mut VecDeque<(Result<T, PreprocError>, Range<usize>)>,
    range: Range<usize>,
    kind: DirectiveKind,
    expr_value: Option<bool>,
    parent_active: bool,
) {
    match kind {
        DirectiveKind::If => {
            let arm_lit = parent_active && expr_value.unwrap_or(false);
            stack.push(Frame {
                if_span: range,
                arm_lit,
                any_arm_lit: arm_lit,
                else_span: None,
            });
        }
        DirectiveKind::Elif => {
            if stack.is_empty() {
                pending.push_back((
                    Err(PreprocError::OrphanElif {
                        range: range.clone(),
                    }),
                    range,
                ));
                return;
            }
            let prev_else = stack.last().and_then(|f| f.else_span.clone());
            if let Some(prev) = prev_else
                && parent_active
            {
                pending.push_back((
                    Err(PreprocError::ElifAfterElse {
                        range: range.clone(),
                        prev_else: prev,
                    }),
                    range.clone(),
                ));
            }
            let len = stack.len();
            let top = &mut stack[len - 1];
            let new_arm_lit = parent_active && !top.any_arm_lit && expr_value.unwrap_or(false);
            top.arm_lit = new_arm_lit;
            top.any_arm_lit |= new_arm_lit;
        }
        DirectiveKind::Else => {
            if stack.is_empty() {
                pending.push_back((
                    Err(PreprocError::OrphanElse {
                        range: range.clone(),
                    }),
                    range,
                ));
                return;
            }
            let prev_else = stack.last().and_then(|f| f.else_span.clone());
            if let Some(prev) = prev_else
                && parent_active
            {
                pending.push_back((
                    Err(PreprocError::DoubleElse {
                        range: range.clone(),
                        prev_else: prev,
                    }),
                    range.clone(),
                ));
            }
            let len = stack.len();
            let top = &mut stack[len - 1];
            let new_arm_lit = parent_active && !top.any_arm_lit;
            top.arm_lit = new_arm_lit;
            top.any_arm_lit |= new_arm_lit;
            top.else_span = Some(range);
        }
        DirectiveKind::EndIf => {
            if stack.is_empty() {
                pending.push_back((
                    Err(PreprocError::UnmatchedEndIf {
                        range: range.clone(),
                    }),
                    range,
                ));
                return;
            }
            stack.pop();
        }
    }
}

// =============================================================================
// Reference implementation (test-only)
// =============================================================================

#[cfg(test)]
mod reference {
    use super::*;

    /// Compute the "active byte mask" for `source` under `symbols`:
    /// `active[i] == true` iff byte `i` is in an active branch and not
    /// inside a directive line. Mirrors the same state transitions as
    /// the fast driver, deliberately by a different code path.
    pub fn active_mask(source: &str, symbols: &HashSet<String>) -> Vec<bool> {
        let bytes = source.as_bytes();
        let mut active = vec![false; bytes.len()];
        let mut stack: Vec<Frame> = Vec::new();
        let mut sink: VecDeque<DriverItem> = VecDeque::new();
        let mut i = 0;
        while i < bytes.len() {
            let is_line_start = i == 0 || matches!(bytes[i - 1], b'\n' | b'\r');
            if is_line_start && let Some(d) = recognise_directive(source, i) {
                // Trivia directives: mark bytes inactive (they aren't real
                // code) and advance, but don't touch the ifdef stack.
                if let Ok(r) = &d
                    && r.directive.is_trivia()
                {
                    for slot in &mut active[r.range.start..r.range.end] {
                        *slot = false;
                    }
                    i = r.range.end;
                    continue;
                }
                let (range, kind, expr_value, suppress_state) = match d {
                    Ok(r) => {
                        let kind = r.directive.kind().expect("trivia handled above");
                        let ev = match r.directive {
                            Directive::If(e) | Directive::Elif(e) => {
                                Some(e.eval(|n| symbols.contains(n)))
                            }
                            _ => None,
                        };
                        (r.range, kind, ev, false)
                    }
                    Err(e) => {
                        let ev = match e.keyword {
                            DirectiveKind::If | DirectiveKind::Elif => Some(false),
                            _ => None,
                        };
                        let suppress = matches!(e.kind, DirectiveErrorKind::MissingSeparator)
                            && matches!(e.keyword, DirectiveKind::If | DirectiveKind::Elif);
                        (e.range, e.keyword, ev, suppress)
                    }
                };
                // Directive bytes are inactive by construction.
                for slot in &mut active[range.start..range.end] {
                    *slot = false;
                }
                if !suppress_state {
                    let pa = parent_active_for(&stack, kind);
                    apply_directive(&mut stack, &mut sink, range.clone(), kind, expr_value, pa);
                }
                i = range.end;
                continue;
            }
            active[i] = stack.iter().all(|f| f.arm_lit);
            i += 1;
        }
        active
    }

    /// Reference lex: drive the bare Logos lexer over `source`, then drop
    /// tokens whose byte range is not entirely active. By construction
    /// the offsets line up with the fast driver's emissions.
    pub fn reference_lex<'a>(
        source: &'a str,
        symbols: &HashSet<String>,
    ) -> Vec<(Result<Token<'a>, LexError>, Range<usize>)> {
        let active = active_mask(source, symbols);
        crate::lexer::lex(source)
            .filter(|(_, span)| span.clone().all(|j| active[j]))
            .collect()
    }

    /// Reference line-directive collector: walk the source line by line,
    /// maintaining the same ifdef stack as the fast driver by a different
    /// code path, and record every `#line` directive seen while the current
    /// branch is active. Valid only for sources without multi-line tokens
    /// (strings / block comments), since it has no tokeniser to skip over a
    /// `#line`-looking line buried inside a string — the same restriction
    /// the [`active_mask`] / [`reference_lex`] oracle carries.
    pub fn collect_line_directives_reference(
        source: &str,
        symbols: &HashSet<String>,
    ) -> LineDirectiveStore {
        let bytes = source.as_bytes();
        let mut stack: Vec<Frame> = Vec::new();
        let mut sink: VecDeque<DriverItem> = VecDeque::new();
        let mut store = LineDirectiveStore::new();
        let mut i = 0;
        while i < bytes.len() {
            let is_line_start = i == 0 || matches!(bytes[i - 1], b'\n' | b'\r');
            if is_line_start && let Some(d) = recognise_directive(source, i) {
                if let Ok(r) = &d
                    && r.directive.is_trivia()
                {
                    if stack.iter().all(|f| f.arm_lit)
                        && let Directive::Line { number, file } = &r.directive
                    {
                        store.push(LineDirective {
                            generated_line: line_index_ref(source, r.range.start),
                            virtual_line: *number,
                            file: file.clone(),
                        });
                    }
                    i = r.range.end;
                    continue;
                }
                let (range, kind, expr_value, suppress_state) = match d {
                    Ok(r) => {
                        let kind = r.directive.kind().expect("trivia handled above");
                        let ev = match r.directive {
                            Directive::If(e) | Directive::Elif(e) => {
                                Some(e.eval(|n| symbols.contains(n)))
                            }
                            _ => None,
                        };
                        (r.range, kind, ev, false)
                    }
                    Err(e) => {
                        let ev = match e.keyword {
                            DirectiveKind::If | DirectiveKind::Elif => Some(false),
                            _ => None,
                        };
                        let suppress = matches!(e.kind, DirectiveErrorKind::MissingSeparator)
                            && matches!(e.keyword, DirectiveKind::If | DirectiveKind::Elif);
                        (e.range, e.keyword, ev, suppress)
                    }
                };
                if !suppress_state {
                    let pa = parent_active_for(&stack, kind);
                    apply_directive(&mut stack, &mut sink, range.clone(), kind, expr_value, pa);
                }
                i = range.end;
                continue;
            }
            i += 1;
        }
        store
    }

    /// Reference enumerator for the full-trivia directive tokens: walk the
    /// source line by line with the same ifdef stack as the fast driver and
    /// record `(SyntaxKind, range)` for every trivia directive seen in an
    /// active branch — the independent counterpart to
    /// `lex_with_symbols_full_trivia`'s emission. Same multi-line-token
    /// caveat as [`collect_line_directives_reference`].
    pub fn collect_trivia_directives_reference(
        source: &str,
        symbols: &HashSet<String>,
    ) -> Vec<(SyntaxKind, Range<usize>)> {
        let bytes = source.as_bytes();
        let mut stack: Vec<Frame> = Vec::new();
        let mut sink: VecDeque<DriverItem> = VecDeque::new();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let is_line_start = i == 0 || matches!(bytes[i - 1], b'\n' | b'\r');
            if is_line_start && let Some(d) = recognise_directive(source, i) {
                if let Ok(r) = &d
                    && r.directive.is_trivia()
                {
                    if stack.iter().all(|f| f.arm_lit) {
                        let kind = match &r.directive {
                            Directive::Line { .. } => SyntaxKind::HASH_LINE,
                            Directive::NoWarn { .. } | Directive::WarnOn { .. } => {
                                SyntaxKind::WARN_DIRECTIVE
                            }
                            _ => unreachable!("is_trivia() ⇒ Line / NoWarn / WarnOn"),
                        };
                        out.push((kind, r.range.clone()));
                    }
                    i = r.range.end;
                    continue;
                }
                let (range, kind, expr_value, suppress_state) = match d {
                    Ok(r) => {
                        let kind = r.directive.kind().expect("trivia handled above");
                        let ev = match r.directive {
                            Directive::If(e) | Directive::Elif(e) => {
                                Some(e.eval(|n| symbols.contains(n)))
                            }
                            _ => None,
                        };
                        (r.range, kind, ev, false)
                    }
                    Err(e) => {
                        let ev = match e.keyword {
                            DirectiveKind::If | DirectiveKind::Elif => Some(false),
                            _ => None,
                        };
                        let suppress = matches!(e.kind, DirectiveErrorKind::MissingSeparator)
                            && matches!(e.keyword, DirectiveKind::If | DirectiveKind::Elif);
                        (e.range, e.keyword, ev, suppress)
                    }
                };
                let pa = parent_active_for(&stack, kind);
                // The driver emits a CC-directive trivia token over the
                // directive line whenever it is visible (parent active),
                // independent of `suppress_state` (a no-separator `#if(FOO)`
                // still surfaces its keyword).
                if pa {
                    let cc_kind = match kind {
                        DirectiveKind::If => SyntaxKind::HASH_IF,
                        DirectiveKind::Elif => SyntaxKind::HASH_ELIF,
                        DirectiveKind::Else => SyntaxKind::HASH_ELSE,
                        DirectiveKind::EndIf => SyntaxKind::HASH_ENDIF,
                    };
                    out.push((cc_kind, range.clone()));
                }
                if !suppress_state {
                    apply_directive(&mut stack, &mut sink, range.clone(), kind, expr_value, pa);
                }
                i = range.end;
                continue;
            }
            i += 1;
        }
        out
    }

    /// Independent 0-based line counter for cross-checking
    /// `line_store::line_index`. Byte-oriented (the production version is
    /// char-oriented), so a divergence in either implementation surfaces as
    /// a `generated_line` mismatch in the line-directive property test.
    fn line_index_ref(source: &str, offset: usize) -> u32 {
        let b = &source.as_bytes()[..offset.min(source.len())];
        let mut line: u32 = 0;
        let mut k = 0;
        while k < b.len() {
            match b[k] {
                b'\r' => {
                    line += 1;
                    if b.get(k + 1) == Some(&b'\n') {
                        k += 1;
                    }
                }
                b'\n' => line += 1,
                _ => {}
            }
            k += 1;
        }
        line
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sym(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn ok_tokens<'a>(items: impl Iterator<Item = DriverItem<'a>>) -> Vec<(String, Range<usize>)> {
        items
            .filter_map(|(tok, span)| match tok {
                Ok(t) => Some((format!("{:?}", t), span)),
                Err(_) => None,
            })
            .collect()
    }

    fn ref_tokens<'a>(
        items: impl Iterator<Item = (Result<Token<'a>, LexError>, Range<usize>)>,
    ) -> Vec<(String, Range<usize>)> {
        items
            .filter_map(|(tok, span)| match tok {
                Ok(t) => Some((format!("{:?}", t), span)),
                Err(_) => None,
            })
            .collect()
    }

    // ---- example tests: active selection -----------------------------------

    #[test]
    fn empty_source_emits_no_tokens() {
        let s = sym(&[]);
        let v: Vec<_> = lex_with_symbols("", &s).collect();
        assert!(v.is_empty());
    }

    #[test]
    fn no_directives_passes_through() {
        let s = sym(&[]);
        let src = "let x = 1\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        assert!(!fast.is_empty());
    }

    #[test]
    fn if_true_arm_is_kept() {
        let s = sym(&["FOO"]);
        let src = "#if FOO\nlet x = 1\n#endif\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        // The active arm has a `let`, so we must see one in the output.
        assert!(fast.iter().any(|(t, _)| t.starts_with("Let")));
    }

    #[test]
    fn if_false_arm_is_dropped() {
        let s = sym(&[]);
        let src = "#if FOO\nlet x = 1\n#endif\nlet y = 2\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        // `let x` should not appear; only `let y`.
        let lets: Vec<_> = fast.iter().filter(|(t, _)| t.starts_with("Let")).collect();
        assert_eq!(lets.len(), 1);
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
    }

    #[test]
    fn else_arm_switches() {
        let s = sym(&[]);
        let src = "#if FOO\nlet x = 1\n#else\nlet y = 2\n#endif\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        // The else arm is selected: `y` should appear, `x` should not.
        let idents: Vec<_> = fast
            .iter()
            .filter(|(t, _)| t.starts_with("Ident"))
            .map(|(t, _)| t.clone())
            .collect();
        assert!(idents.iter().any(|t| t.contains("y")));
        assert!(!idents.iter().any(|t| t.contains("\"x\"")));
    }

    #[test]
    fn elif_first_matching_wins() {
        let s = sym(&["B"]);
        let src = "#if A\nlet x = 1\n#elif B\nlet y = 2\n#else\nlet z = 3\n#endif\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        let idents: Vec<_> = fast
            .iter()
            .filter(|(t, _)| t.starts_with("Ident"))
            .map(|(t, _)| t.clone())
            .collect();
        assert!(idents.iter().any(|t| t.contains("\"y\"")));
        assert!(!idents.iter().any(|t| t.contains("\"x\"")));
        assert!(!idents.iter().any(|t| t.contains("\"z\"")));
    }

    #[test]
    fn elif_all_false_falls_to_else() {
        let s = sym(&[]);
        let src = "#if A\nlet x = 1\n#elif B\nlet y = 2\n#else\nlet z = 3\n#endif\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        let idents: Vec<_> = fast
            .iter()
            .filter(|(t, _)| t.starts_with("Ident"))
            .map(|(t, _)| t.clone())
            .collect();
        assert!(idents.iter().any(|t| t.contains("\"z\"")));
        assert!(!idents.iter().any(|t| t.contains("\"x\"")));
        assert!(!idents.iter().any(|t| t.contains("\"y\"")));
    }

    #[test]
    fn nested_ifs_both_active() {
        let s = sym(&["A", "B"]);
        let src = "#if A\n#if B\nlet x = 1\n#endif\n#endif\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        assert!(fast.iter().any(|(t, _)| t.starts_with("Let")));
    }

    #[test]
    fn nested_inactive_inside_active_is_skipped() {
        let s = sym(&["A"]);
        let src = "#if A\n#if B\nlet x = 1\n#endif\nlet y = 2\n#endif\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        let idents: Vec<_> = fast
            .iter()
            .filter(|(t, _)| t.starts_with("Ident"))
            .map(|(t, _)| t.clone())
            .collect();
        assert!(idents.iter().any(|t| t.contains("\"y\"")));
        assert!(!idents.iter().any(|t| t.contains("\"x\"")));
    }

    #[test]
    fn nested_inside_inactive_is_skipped_entirely() {
        // The outer `#if A` is false → everything inside (including the
        // inner `#if B ... #endif`) must be skipped; only `z` survives.
        let s = sym(&["B"]);
        let src = "#if A\n#if B\nlet x = 1\n#endif\n#endif\nlet z = 3\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        let idents: Vec<_> = fast
            .iter()
            .filter(|(t, _)| t.starts_with("Ident"))
            .map(|(t, _)| t.clone())
            .collect();
        assert!(idents.iter().any(|t| t.contains("\"z\"")));
        assert!(!idents.iter().any(|t| t.contains("\"x\"")));
    }

    // ---- trivia directives -------------------------------------------------

    #[test]
    fn nowarn_line_is_swallowed_without_tokens() {
        // `#nowarn "40"` must not surface as `Hash` + `Ident("nowarn")` +
        // `String("40")` — the driver intercepts and discards the whole line.
        let s = sym(&[]);
        let src = "#nowarn \"40\"\nlet x = 1\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        // No `Hash` tokens should reach the output.
        let has_hash = fast.iter().any(|(t, _)| matches!(t, Ok(Token::Hash)));
        assert!(!has_hash, "Hash token leaked through: {:?}", fast);
        // No `String` token from the `"40"` body should reach the output.
        let has_string = fast.iter().any(|(t, _)| matches!(t, Ok(Token::String)));
        assert!(!has_string, "String token leaked through: {:?}", fast);
        // The `let x = 1` after the directive line still tokenises.
        let has_let = fast.iter().any(|(t, _)| matches!(t, Ok(Token::Let)));
        assert!(has_let, "Let after #nowarn missing: {:?}", fast);
        // No preprocessor errors.
        let errs: Vec<_> = fast.iter().filter_map(|(t, _)| t.clone().err()).collect();
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn warnon_line_is_swallowed() {
        let s = sym(&[]);
        let src = "#warnon \"3218\"\nlet x = 1\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        assert!(!fast.iter().any(|(t, _)| matches!(t, Ok(Token::Hash))));
        let errs: Vec<_> = fast.iter().filter_map(|(t, _)| t.clone().err()).collect();
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn line_directive_is_swallowed() {
        // `#line 5 "foo.fs"` — must not produce a `String` token from the
        // file-name payload, nor a Hash from the leading `#`.
        let s = sym(&[]);
        let src = "#line 5 \"foo.fs\"\nlet x = 1\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        assert!(!fast.iter().any(|(t, _)| matches!(t, Ok(Token::Hash))));
        assert!(!fast.iter().any(|(t, _)| matches!(t, Ok(Token::String))));
        assert!(fast.iter().any(|(t, _)| matches!(t, Ok(Token::Let))));
    }

    #[test]
    fn bare_numeric_line_directive_is_swallowed() {
        // Generated `fsyacclex.fs`-style `# 1 "fsyacclex.fsl"` — the bare-
        // numeric `#line` alternate FCS recognises via the `'#' anywhite*
        // digit+ ...` rule.
        let s = sym(&[]);
        let src = "# 1 \"fsyacclex.fsl\"\nlet x = 1\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        assert!(!fast.iter().any(|(t, _)| matches!(t, Ok(Token::Hash))));
        assert!(!fast.iter().any(|(t, _)| matches!(t, Ok(Token::String))));
        assert!(fast.iter().any(|(t, _)| matches!(t, Ok(Token::Let))));
    }

    #[test]
    fn trivia_directive_does_not_open_a_frame() {
        // A `#nowarn` line inside the source must not affect the ifdef
        // stack: a subsequent `let` still tokenises in active mode.
        let s = sym(&[]);
        let src = "#nowarn \"40\"\n#nowarn \"42\"\nlet x = 1\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        let errs: Vec<_> = fast.iter().filter_map(|(t, _)| t.clone().err()).collect();
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, PreprocError::UnclosedIfAtEof { .. })),
            "trivia directive opened a frame: {:?}",
            errs
        );
        let lets = fast
            .iter()
            .filter(|(t, _)| matches!(t, Ok(Token::Let)))
            .count();
        assert_eq!(lets, 1);
    }

    #[test]
    fn trivia_inside_inactive_arm_is_skipped_with_the_body() {
        // Inside `#if FOO` (false), a `#nowarn` line is byte-skipped along
        // with everything else in the arm. The driver doesn't need to
        // intercept it — but if it does, the outcome is the same.
        let s = sym(&[]);
        let src = "#if FOO\n#nowarn \"40\"\nlet x = 1\n#endif\nlet y = 2\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        let lets = fast
            .iter()
            .filter(|(t, _)| matches!(t, Ok(Token::Let)))
            .count();
        assert_eq!(lets, 1, "expected only `let y`: {:?}", fast);
        let errs: Vec<_> = fast.iter().filter_map(|(t, _)| t.clone().err()).collect();
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    // ---- full-trivia mode (FCS skip=false) ---------------------------------

    /// The full-trivia stream's directive-trivia tokens as `(kind, range)`.
    fn full_trivia_directives(
        src: &str,
        symbols: &HashSet<String>,
    ) -> Vec<(SyntaxKind, Range<usize>)> {
        lex_with_symbols_full_trivia(src, symbols)
            .filter_map(|(res, span)| match res {
                Ok(tt) => tt.trivia_syntax_kind().map(|k| (k, span)),
                Err(_) => None,
            })
            .collect()
    }

    #[test]
    fn trivia_token_syntax_kind_mapping() {
        assert_eq!(
            TriviaToken::HashLine.trivia_syntax_kind(),
            Some(SyntaxKind::HASH_LINE)
        );
        assert_eq!(
            TriviaToken::WarnDirective.trivia_syntax_kind(),
            Some(SyntaxKind::WARN_DIRECTIVE)
        );
        assert_eq!(
            TriviaToken::HashIf.trivia_syntax_kind(),
            Some(SyntaxKind::HASH_IF)
        );
        assert_eq!(
            TriviaToken::HashElse.trivia_syntax_kind(),
            Some(SyntaxKind::HASH_ELSE)
        );
        assert_eq!(
            TriviaToken::HashElif.trivia_syntax_kind(),
            Some(SyntaxKind::HASH_ELIF)
        );
        assert_eq!(
            TriviaToken::HashEndif.trivia_syntax_kind(),
            Some(SyntaxKind::HASH_ENDIF)
        );
        assert_eq!(
            TriviaToken::InactiveCode.trivia_syntax_kind(),
            Some(SyntaxKind::INACTIVECODE)
        );
        assert_eq!(TriviaToken::Lexed(Token::Let).trivia_syntax_kind(), None);
    }

    #[test]
    fn full_trivia_emits_warn_directive_for_nowarn() {
        let s = sym(&[]);
        let got = full_trivia_directives("#nowarn \"40\"\nlet x = 1\n", &s);
        assert_eq!(got, vec![(SyntaxKind::WARN_DIRECTIVE, 0..12)]);
    }

    #[test]
    fn full_trivia_emits_warn_directive_for_warnon() {
        let s = sym(&[]);
        let got = full_trivia_directives("#warnon \"3218\"\nlet x = 1\n", &s);
        assert_eq!(got, vec![(SyntaxKind::WARN_DIRECTIVE, 0..14)]);
    }

    #[test]
    fn full_trivia_emits_hash_line_for_line() {
        let s = sym(&[]);
        let got = full_trivia_directives("#line 5 \"foo.fs\"\nlet x = 1\n", &s);
        assert_eq!(got, vec![(SyntaxKind::HASH_LINE, 0..16)]);
    }

    #[test]
    fn full_trivia_emits_hash_line_for_bare_numeric() {
        let s = sym(&[]);
        let got = full_trivia_directives("# 1 \"fsyacclex.fsl\"\nlet x = 1\n", &s);
        assert_eq!(got, vec![(SyntaxKind::HASH_LINE, 0..19)]);
    }

    #[test]
    fn full_trivia_directive_in_active_if_is_emitted() {
        let s = sym(&["FOO"]);
        // Active `#if FOO` branch: the `#nowarn` line (byte 8, after
        // `#if FOO\n`) surfaces as a WARN_DIRECTIVE alongside the bounding
        // HASH_IF / HASH_ENDIF.
        let got = full_trivia_directives("#if FOO\n#nowarn \"40\"\nlet x = 1\n#endif\n", &s);
        assert_eq!(
            got,
            vec![
                (SyntaxKind::HASH_IF, 0..7),
                (SyntaxKind::WARN_DIRECTIVE, 8..20),
                (SyntaxKind::HASH_ENDIF, 31..37),
            ]
        );
    }

    #[test]
    fn full_trivia_directive_in_dead_if_is_suppressed() {
        let s = sym(&[]);
        // `#if BAR` (undefined) → dead then-branch: the `#nowarn` inside it
        // is *not* emitted as a WARN_DIRECTIVE (the compiler never sees it).
        // The bounding HASH_IF / HASH_ENDIF and the dead region's
        // INACTIVECODE are still present.
        let got = full_trivia_directives("#if BAR\n#nowarn \"40\"\n#endif\nlet y = 2\n", &s);
        assert!(
            !got.iter().any(|(k, _)| *k == SyntaxKind::WARN_DIRECTIVE),
            "dead-branch #nowarn surfaced: {:?}",
            got
        );
        assert_eq!(got[0], (SyntaxKind::HASH_IF, 0..7));
    }

    #[test]
    fn full_trivia_token_covers_leading_whitespace() {
        let s = sym(&[]);
        // The recognised range starts at the line start, including the
        // leading spaces before `#` (FCS's `anywhite*`).
        let got = full_trivia_directives("   #nowarn \"40\"\nlet x = 1\n", &s);
        assert_eq!(got, vec![(SyntaxKind::WARN_DIRECTIVE, 0..15)]);
    }

    #[test]
    fn full_trivia_passes_through_real_tokens() {
        let s = sym(&[]);
        // Non-directive code still lexes as `Lexed` tokens.
        let toks: Vec<_> = lex_with_symbols_full_trivia("#nowarn \"40\"\nlet x = 1\n", &s)
            .filter_map(|(res, _)| match res {
                Ok(TriviaToken::Lexed(t)) => Some(t),
                _ => None,
            })
            .collect();
        assert!(toks.iter().any(|t| matches!(t, Token::Let)));
    }

    #[test]
    fn full_trivia_partitions_if_else_endif() {
        // `#if FOO` undefined → the then-branch is dead (one INACTIVECODE
        // covering its `\nlet x = 1\n`), the `#else` branch active.
        let s = sym(&[]);
        let got = full_trivia_directives("#if FOO\nlet x = 1\n#else\nlet y = 2\n#endif\n", &s);
        assert_eq!(
            got,
            vec![
                (SyntaxKind::HASH_IF, 0..7),
                (SyntaxKind::INACTIVECODE, 7..18),
                (SyntaxKind::HASH_ELSE, 18..23),
                (SyntaxKind::HASH_ENDIF, 34..40),
            ]
        );
    }

    #[test]
    fn full_trivia_nested_dead_region_is_one_inactive_span() {
        // FOO undefined → the whole then-branch is dead. The nested
        // `#if BAR … #endif` inside it is *not* separately tokenised: it
        // collapses into the single enclosing INACTIVECODE (7..33), bounded
        // by the visible outer HASH_IF / HASH_ENDIF.
        let s = sym(&[]);
        let src = "#if FOO\n#if BAR\nlet x = 1\n#endif\n#endif\nlet y = 2\n";
        let got = full_trivia_directives(src, &s);
        assert_eq!(
            got,
            vec![
                (SyntaxKind::HASH_IF, 0..7),
                (SyntaxKind::INACTIVECODE, 7..33),
                (SyntaxKind::HASH_ENDIF, 33..39),
            ]
        );
    }

    #[test]
    fn full_trivia_dead_branch_malformed_bytes_are_inactive_not_lexed() {
        // The whole point of the directive layer: an unterminated `(*` in a
        // dead branch is covered by INACTIVECODE and never reaches the lexer.
        let s = sym(&[]);
        let src = "#if FOO\n(* unterminated\n#endif\nlet y = 2\n";
        let items: Vec<_> = lex_with_symbols_full_trivia(src, &s).collect();
        assert!(
            !items.iter().any(|(res, _)| res.is_err()),
            "dead branch was lexed: {:?}",
            items
        );
        assert_eq!(
            full_trivia_directives(src, &s),
            vec![
                (SyntaxKind::HASH_IF, 0..7),
                (SyntaxKind::INACTIVECODE, 7..24),
                (SyntaxKind::HASH_ENDIF, 24..30),
            ]
        );
        assert!(
            items
                .iter()
                .any(|(res, _)| matches!(res, Ok(TriviaToken::Lexed(Token::Let)))),
            "`let y = 2` after the dead branch did not lex: {:?}",
            items
        );
    }

    // ---- #line directive capture -------------------------------------------

    fn collect_store(src: &str, symbols: &HashSet<String>) -> LineDirectiveStore {
        let mut driver = lex_with_symbols(src, symbols);
        for _ in driver.by_ref() {}
        driver.line_directives().clone()
    }

    #[test]
    fn line_directive_with_file_is_captured() {
        let s = sym(&[]);
        let store = collect_store("#line 5 \"foo.fs\"\nlet x = 1\n", &s);
        assert_eq!(
            store.directives(),
            &[LineDirective {
                generated_line: 0,
                virtual_line: 5,
                file: Some("foo.fs".to_string()),
            }]
        );
    }

    #[test]
    fn bare_numeric_line_directive_is_captured() {
        // fslex/fsyacc emit the `# N "file"` alternate.
        let s = sym(&[]);
        let store = collect_store("# 1 \"fsyacclex.fsl\"\nlet x = 1\n", &s);
        assert_eq!(
            store.directives(),
            &[LineDirective {
                generated_line: 0,
                virtual_line: 1,
                file: Some("fsyacclex.fsl".to_string()),
            }]
        );
    }

    #[test]
    fn line_directive_without_file_is_captured() {
        let s = sym(&[]);
        let store = collect_store("#line 10\nlet x = 1\n", &s);
        assert_eq!(
            store.directives(),
            &[LineDirective {
                generated_line: 0,
                virtual_line: 10,
                file: None,
            }]
        );
    }

    #[test]
    fn line_directive_in_active_branch_is_captured() {
        let s = sym(&["FOO"]);
        let store = collect_store("#if FOO\n#line 5 \"a.fs\"\nlet x = 1\n#endif\n", &s);
        assert_eq!(
            store.directives(),
            &[LineDirective {
                generated_line: 1,
                virtual_line: 5,
                file: Some("a.fs".to_string()),
            }]
        );
    }

    #[test]
    fn line_directive_in_inactive_branch_is_not_captured() {
        // The F# compiler never sees a `#line` in a dead branch, so it must
        // not take effect.
        let s = sym(&[]);
        let store = collect_store("#if FOO\n#line 5 \"a.fs\"\n#endif\nlet y = 2\n", &s);
        assert!(store.is_empty(), "dead-branch #line captured: {:?}", store);
    }

    #[test]
    fn multiple_line_directives_recorded_in_source_order() {
        let s = sym(&[]);
        let store = collect_store(
            "#line 100 \"a.fs\"\nlet x = 1\n#line 200 \"b.fs\"\nlet y = 2\n",
            &s,
        );
        assert_eq!(
            store.directives(),
            &[
                LineDirective {
                    generated_line: 0,
                    virtual_line: 100,
                    file: Some("a.fs".to_string()),
                },
                LineDirective {
                    generated_line: 2,
                    virtual_line: 200,
                    file: Some("b.fs".to_string()),
                },
            ]
        );
    }

    #[test]
    fn line_directive_as_last_line_is_captured() {
        let s = sym(&[]);
        let store = collect_store("let x = 1\n#line 7 \"z.fs\"\n", &s);
        assert_eq!(
            store.directives(),
            &[LineDirective {
                generated_line: 1,
                virtual_line: 7,
                file: Some("z.fs".to_string()),
            }]
        );
    }

    #[test]
    fn eof_terminated_line_directive_is_not_captured() {
        // FCS's `#line` rule anchors on a trailing newline; a directive that
        // runs straight into EOF is not recognised, so nothing is captured.
        let s = sym(&[]);
        let store = collect_store("let x = 1\n#line 7 \"z.fs\"", &s);
        assert!(
            store.is_empty(),
            "EOF-terminated #line captured: {:?}",
            store
        );
    }

    #[test]
    fn nowarn_and_warnon_do_not_populate_the_store() {
        let s = sym(&[]);
        let store = collect_store("#nowarn \"40\"\n#warnon \"42\"\nlet x = 1\n", &s);
        assert!(store.is_empty(), "non-#line trivia captured: {:?}", store);
    }

    // ---- multi-line tokens straddling directives ---------------------------

    #[test]
    fn triple_string_containing_pseudo_directive_is_not_intercepted() {
        // Triple-quoted string contains text that *looks* like an `#endif`
        // at column 0 — but Logos emits the whole string as one token,
        // and the directive layer only inspects bytes *between* tokens.
        let s = sym(&["FOO"]);
        let src = "#if FOO\nlet x = \"\"\"hi\n#endif inside\"\"\"\n#endif\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        // Should be: Let, WS, Ident("x"), WS, Equals, WS, TripleString, Newline.
        let ok_count = fast.iter().filter(|(t, _)| t.is_ok()).count();
        assert!(ok_count > 0);
        let has_triple = fast
            .iter()
            .any(|(t, _)| matches!(t, Ok(Token::TripleString)));
        assert!(has_triple, "expected a TripleString token");
        // And the outer `#endif` line should close the frame: no
        // unclosed-if errors.
        let errs: Vec<_> = fast
            .iter()
            .filter_map(|(t, _)| match t {
                Err(e) => Some(e.clone()),
                Ok(_) => None,
            })
            .collect();
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn block_comment_in_inactive_branch_is_not_lexed() {
        // The unterminated `(*` would crash the bare lexer, but it's in
        // a skipped branch so the driver never sees it.
        let s = sym(&[]);
        let src = "#if FOO\n(* unterminated\n#endif\nlet y = 2\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        // We should still get the `let y = 2` after the closing #endif.
        let has_let = fast.iter().any(|(t, _)| matches!(t, Ok(Token::Let)));
        assert!(has_let, "expected Let token; got {:?}", fast);
        // And no lexer error.
        let lex_errs: Vec<_> = fast
            .iter()
            .filter_map(|(t, _)| match t {
                Err(PreprocError::Lex(_)) => Some(()),
                _ => None,
            })
            .collect();
        assert!(lex_errs.is_empty(), "unexpected lex errors");
    }

    #[test]
    fn unterminated_string_in_inactive_branch_is_not_lexed() {
        let s = sym(&[]);
        let src = "#if FOO\nlet bad = \"oops\n#endif\nlet y = 2\n";
        let fast: Vec<_> = lex_with_symbols(src, &s).collect();
        let has_let = fast.iter().any(|(t, _)| matches!(t, Ok(Token::Let)));
        assert!(has_let);
        let lex_errs: Vec<_> = fast
            .iter()
            .filter_map(|(t, _)| match t {
                Err(PreprocError::Lex(_)) => Some(()),
                _ => None,
            })
            .collect();
        assert!(lex_errs.is_empty(), "unexpected lex errors");
    }

    // ---- error paths -------------------------------------------------------

    #[test]
    fn unmatched_endif_is_reported_and_skipped() {
        let s = sym(&[]);
        let src = "let x = 1\n#endif\nlet y = 2\n";
        let items: Vec<_> = lex_with_symbols(src, &s).collect();
        let errs: Vec<_> = items
            .iter()
            .filter_map(|(t, _)| match t {
                Err(e) => Some(e.clone()),
                Ok(_) => None,
            })
            .collect();
        assert_eq!(errs.len(), 1);
        assert!(matches!(errs[0], PreprocError::UnmatchedEndIf { .. }));
        // `let y = 2` still tokenises.
        let lets = items
            .iter()
            .filter(|(t, _)| matches!(t, Ok(Token::Let)))
            .count();
        assert_eq!(lets, 2);
    }

    #[test]
    fn orphan_else_is_reported() {
        let s = sym(&[]);
        let src = "let x = 1\n#else\nlet y = 2\n";
        let errs: Vec<_> = lex_with_symbols(src, &s)
            .filter_map(|(t, _)| t.err())
            .collect();
        assert!(matches!(errs[0], PreprocError::OrphanElse { .. }));
    }

    #[test]
    fn orphan_elif_is_reported() {
        let s = sym(&[]);
        let src = "let x = 1\n#elif FOO\nlet y = 2\n";
        let errs: Vec<_> = lex_with_symbols(src, &s)
            .filter_map(|(t, _)| t.err())
            .collect();
        assert!(matches!(errs[0], PreprocError::OrphanElif { .. }));
    }

    #[test]
    fn double_else_is_reported() {
        let s = sym(&[]);
        let src = "#if FOO\n#else\nlet x = 1\n#else\nlet y = 2\n#endif\n";
        let errs: Vec<_> = lex_with_symbols(src, &s)
            .filter_map(|(t, _)| t.err())
            .collect();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PreprocError::DoubleElse { .. })),
            "no DoubleElse in {:?}",
            errs
        );
    }

    #[test]
    fn elif_after_else_is_reported() {
        let s = sym(&[]);
        let src = "#if FOO\n#else\nlet x = 1\n#elif BAR\nlet y = 2\n#endif\n";
        let errs: Vec<_> = lex_with_symbols(src, &s)
            .filter_map(|(t, _)| t.err())
            .collect();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PreprocError::ElifAfterElse { .. })),
            "no ElifAfterElse in {:?}",
            errs
        );
    }

    #[test]
    fn double_else_in_inactive_parent_is_suppressed() {
        // The inner chain sits inside a false outer `#if`. Its two
        // `#else` lines are depth-skipped, not diagnosed — FCS behaviour.
        let s = sym(&[]);
        let src = "#if FOO\n#if BAR\n#else\n#else\n#endif\n#endif\n";
        let errs: Vec<_> = lex_with_symbols(src, &s)
            .filter_map(|(t, _)| t.err())
            .collect();
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, PreprocError::DoubleElse { .. })),
            "should not emit DoubleElse for chain inside inactive parent, got: {:?}",
            errs
        );
    }

    #[test]
    fn elif_after_else_in_inactive_parent_is_suppressed() {
        let s = sym(&[]);
        let src = "#if FOO\n#if BAR\n#else\n#elif BAZ\n#endif\n#endif\n";
        let errs: Vec<_> = lex_with_symbols(src, &s)
            .filter_map(|(t, _)| t.err())
            .collect();
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, PreprocError::ElifAfterElse { .. })),
            "should not emit ElifAfterElse for chain inside inactive parent, got: {:?}",
            errs
        );
    }

    #[test]
    fn malformed_directive_in_inactive_parent_is_suppressed() {
        // A `#if !` (empty body) inside an inactive outer `#if` is a
        // parse error, but the directive layer is depth-skipping and
        // FCS does not diagnose it. We follow suit.
        let s = sym(&[]);
        let src = "#if FOO\n#if !\nlet x = 1\n#endif\n#endif\nlet y = 2\n";
        let errs: Vec<_> = lex_with_symbols(src, &s)
            .filter_map(|(t, _)| t.err())
            .collect();
        assert!(
            !errs.iter().any(|e| matches!(e, PreprocError::Directive(_))),
            "should not emit Directive error inside inactive parent, got: {:?}",
            errs
        );
    }

    #[test]
    fn unclosed_if_at_eof_is_reported() {
        let s = sym(&[]);
        let src = "#if FOO\nlet x = 1\n";
        let errs: Vec<_> = lex_with_symbols(src, &s)
            .filter_map(|(t, _)| t.err())
            .collect();
        assert_eq!(errs.len(), 1);
        assert!(matches!(errs[0], PreprocError::UnclosedIfAtEof { .. }));
    }

    #[test]
    fn malformed_if_body_is_directive_error_and_skipped() {
        let s = sym(&[]);
        // `#if !` is a parse error in the body. The driver treats it as
        // `#if false` and emits a Directive error.
        let src = "#if !\nlet x = 1\n#endif\nlet y = 2\n";
        let items: Vec<_> = lex_with_symbols(src, &s).collect();
        let has_directive_err = items
            .iter()
            .any(|(t, _)| matches!(t, Err(PreprocError::Directive(_))));
        assert!(has_directive_err);
        // `let x` is skipped; `let y` survives.
        let lets = items
            .iter()
            .filter(|(t, _)| matches!(t, Ok(Token::Let)))
            .count();
        assert_eq!(lets, 1);
    }

    #[test]
    fn bare_if_at_eof_does_not_open_a_frame() {
        // Mirrors `E_UnmatchedIf01.fs`. FCS emits "must have ident" but
        // doesn't enter a conditional section, so no `UnclosedIfAtEof`.
        let s = sym(&[]);
        let src = "#if\n";
        let items: Vec<_> = lex_with_symbols(src, &s).collect();
        let errs: Vec<_> = items.iter().filter_map(|(t, _)| t.clone().err()).collect();
        let has_directive_err = errs.iter().any(|e| matches!(e, PreprocError::Directive(_)));
        assert!(
            has_directive_err,
            "expected Directive error, got {:?}",
            errs
        );
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, PreprocError::UnclosedIfAtEof { .. })),
            "should not emit UnclosedIfAtEof for bare #if, got {:?}",
            errs
        );
    }

    #[test]
    fn if_with_no_separator_does_not_open_a_frame() {
        // `#if(FOO)` is invalid (FCS requires `anywhite+` between `#if`
        // and the body). We emit the directive error and resume lexing
        // in the surrounding mode — the `(`, `FOO`, `)` on the same
        // line tokenise normally, and subsequent lines stay active.
        let s = sym(&[]);
        let src = "#if(FOO)\nlet x = 1\nlet y = 2\n";
        let items: Vec<_> = lex_with_symbols(src, &s).collect();
        let errs: Vec<_> = items.iter().filter_map(|(t, _)| t.clone().err()).collect();
        assert!(
            errs.iter().any(|e| matches!(e, PreprocError::Directive(_))),
            "expected Directive error, got {:?}",
            errs
        );
        // Both `let x` and `let y` should tokenise — we did not skip
        // into a phantom conditional section.
        let lets = items
            .iter()
            .filter(|(t, _)| matches!(t, Ok(Token::Let)))
            .count();
        assert_eq!(
            lets, 2,
            "expected two Lets (both lines active); items: {:?}",
            items
        );
        // The trailing `(FOO)` must still appear as real tokens — the
        // missing-separator recovery only consumes the keyword.
        let toks: Vec<_> = items.iter().filter_map(|(t, _)| t.clone().ok()).collect();
        assert!(
            toks.iter().any(|t| matches!(t, Token::LParen)),
            "expected an LParen from `(FOO)`; got {:?}",
            toks
        );
        assert!(
            toks.iter().any(|t| matches!(t, Token::Ident("FOO"))),
            "expected `Ident(\"FOO\")` from `(FOO)`; got {:?}",
            toks
        );
        assert!(
            toks.iter().any(|t| matches!(t, Token::RParen)),
            "expected an RParen from `(FOO)`; got {:?}",
            toks
        );
    }

    #[test]
    fn bare_elif_does_not_change_arm_state() {
        // `#if FOO` is true; an inner bare `#elif` must not deactivate
        // the arm. FCS reports the missing-ident error but leaves the
        // ifdef stack alone.
        let s = sym(&["FOO"]);
        let src = "#if FOO\nlet x = 1\n#elif\nlet y = 2\n#endif\n";
        let items: Vec<_> = lex_with_symbols(src, &s).collect();
        let errs: Vec<_> = items.iter().filter_map(|(t, _)| t.clone().err()).collect();
        assert!(
            errs.iter().any(|e| matches!(e, PreprocError::Directive(_))),
            "expected Directive error, got {:?}",
            errs
        );
        // The original arm stays selected, so both `let x` and `let y`
        // appear.
        let lets = items
            .iter()
            .filter(|(t, _)| matches!(t, Ok(Token::Let)))
            .count();
        assert_eq!(lets, 2);
    }

    #[test]
    fn if_with_separator_but_empty_body_opens_false_frame() {
        // `#if   ` matches the FCS `#if anywhite+ anystring` rule, so it
        // *does* open a conditional frame (with `false`). The body inside
        // is skipped — including an unterminated `"` that would crash the
        // bare lexer — and the matching `#endif` closes the frame.
        let s = sym(&[]);
        let src = "#if   \nlet bad = \"oops\n#endif\nlet y = 2\n";
        let items: Vec<_> = lex_with_symbols(src, &s).collect();
        let errs: Vec<_> = items.iter().filter_map(|(t, _)| t.clone().err()).collect();
        assert!(
            errs.iter().any(|e| matches!(e, PreprocError::Directive(_))),
            "expected Directive error, got {:?}",
            errs
        );
        // No UnmatchedEndIf — the frame absorbed the closer.
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, PreprocError::UnmatchedEndIf { .. })),
            "should not report UnmatchedEndIf — frame was opened, got {:?}",
            errs
        );
        // No lexer error from the unterminated string — body was skipped.
        assert!(
            !errs.iter().any(|e| matches!(e, PreprocError::Lex(_))),
            "should not lex the inactive body, got {:?}",
            errs
        );
        // `let y` after the `#endif` survives.
        let lets = items
            .iter()
            .filter(|(t, _)| matches!(t, Ok(Token::Let)))
            .count();
        assert_eq!(lets, 1);
    }

    #[test]
    fn elif_with_separator_but_empty_body_deactivates_arm() {
        // After a true `#if A`, a malformed `#elif   ` (with separator,
        // empty body) reaches the body-evaluation path and is treated as
        // `#elif false`. Combined with the rule "no further arm may light
        // once one has", the chain stays inactive after this point.
        let s = sym(&["A"]);
        let src = "#if A\nlet x = 1\n#elif   \nlet y = 2\n#endif\n";
        let items: Vec<_> = lex_with_symbols(src, &s).collect();
        let errs: Vec<_> = items.iter().filter_map(|(t, _)| t.clone().err()).collect();
        assert!(
            errs.iter().any(|e| matches!(e, PreprocError::Directive(_))),
            "expected Directive error, got {:?}",
            errs
        );
        // `let x` is the selected arm; `let y` is in the `#elif` arm and
        // does not run (the chain already burned its single light on `A`).
        let lets = items
            .iter()
            .filter(|(t, _)| matches!(t, Ok(Token::Let)))
            .count();
        assert_eq!(lets, 1);
    }

    // ---- corpus fixtures ---------------------------------------------------

    fn read_fixture(name: &str) -> Option<String> {
        let base = std::env::var("BORZOI_CORPUS").ok()?;
        let path = std::path::PathBuf::from(base).join(
            format!(
                "tests/FSharp.Compiler.ComponentTests/resources/tests/Conformance/LexicalAnalysis/ConditionalCompilation/{}",
                name
            ),
        );
        std::fs::read_to_string(path).ok()
    }

    #[test]
    fn fixture_nested_01() {
        let Some(src) = read_fixture("Nested01.fs") else {
            return;
        };
        let s = sym(&["DEFINED1", "DEFINED2"]);
        // Driver should consume the file without preprocessor errors and
        // agree with the reference token stream.
        let fast: Vec<_> = lex_with_symbols(&src, &s).collect();
        let errs: Vec<_> = fast
            .iter()
            .filter_map(|(t, _)| match t {
                Err(e) => Some(e.clone()),
                Ok(_) => None,
            })
            .collect();
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
        let fast_toks = ok_tokens(fast.into_iter());
        let reference = ref_tokens(reference::reference_lex(&src, &s).into_iter());
        assert_eq!(fast_toks, reference);
    }

    #[test]
    fn fixture_nested_02() {
        let Some(src) = read_fixture("Nested02.fs") else {
            return;
        };
        let s = sym(&["DEFINED1", "DEFINED2", "DEFINED3"]);
        let fast: Vec<_> = lex_with_symbols(&src, &s).collect();
        let errs: Vec<_> = fast
            .iter()
            .filter_map(|(t, _)| match t {
                Err(e) => Some(e.clone()),
                Ok(_) => None,
            })
            .collect();
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
        let fast_toks = ok_tokens(fast.into_iter());
        let reference = ref_tokens(reference::reference_lex(&src, &s).into_iter());
        assert_eq!(fast_toks, reference);
    }

    #[test]
    fn fixture_in_comment_01_lexes_cleanly() {
        // The whole point of the directive layer: an unterminated `(*`
        // sits in an inactive branch and the lexer never sees it.
        let Some(src) = read_fixture("InComment01.fs") else {
            return;
        };
        let s = sym(&[]);
        let items: Vec<_> = lex_with_symbols(&src, &s).collect();
        let lex_errs: Vec<_> = items
            .iter()
            .filter_map(|(t, _)| match t {
                Err(PreprocError::Lex(e)) => Some(e.clone()),
                _ => None,
            })
            .collect();
        assert!(lex_errs.is_empty(), "lex errors: {:?}", lex_errs);
    }

    /// `#if`/`#endif` lines crossing the inside of an interpolated-string
    /// fill. The directive driver invalidates the inner lexer at each
    /// directive boundary; without snapshotting the [`InterpDriver`]'s
    /// frame stack, the rebuilt lexer would lose track of the open fill
    /// and tokenise the closing `}` as a plain `RBrace`. The reference
    /// `crate::lexer::lex` keeps the fill alive (no recreation involved),
    /// so any divergence here is the fast driver dropping frame state.
    #[test]
    fn interp_fill_straddling_directive_preserves_frames() {
        let s = sym(&[]);
        let src = "$\"{\n#if FOO\n1\n#endif\n}\"\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
        // Sanity-check that the End token actually made it through both
        // pipelines — if the frame stack were dropped, fast would emit
        // an `RBrace` here instead.
        assert!(
            fast.iter().any(|(t, _)| t.contains("InterpString(End")),
            "fast driver missing interp End token: {:?}",
            fast
        );
    }

    /// Same as above but with the `#if` arm selected (`FOO` defined).
    /// The fill body's `1` survives, but the directive boundaries still
    /// invalidate the lexer at the `#if` and `#endif` lines, so the
    /// frame snapshot/restore must be exercised twice. Reference still
    /// sees a single continuous fill — divergence would indicate the
    /// fast driver lost frames on either invalidation.
    #[test]
    fn interp_fill_straddling_active_directive_preserves_frames() {
        let s = sym(&["FOO"]);
        let src = "$\"{\n#if FOO\n1\n#endif\n}\"\n";
        let fast = ok_tokens(lex_with_symbols(src, &s));
        let reference = ref_tokens(reference::reference_lex(src, &s).into_iter());
        assert_eq!(fast, reference);
    }

    #[test]
    fn fixture_in_string_literal_03_lexes_cleanly() {
        let Some(src) = read_fixture("InStringLiteral03.fs") else {
            return;
        };
        let s = sym(&[]);
        let items: Vec<_> = lex_with_symbols(&src, &s).collect();
        let lex_errs: Vec<_> = items
            .iter()
            .filter_map(|(t, _)| match t {
                Err(PreprocError::Lex(e)) => Some(e.clone()),
                _ => None,
            })
            .collect();
        assert!(lex_errs.is_empty(), "lex errors: {:?}", lex_errs);
    }

    // ---- property tests ----------------------------------------------------

    /// A "balanced" source: a sequence of content lines, trivia directives
    /// (`#line` / `#nowarn` / `#warnon`), and well-formed `#if … #endif`
    /// blocks (optionally with `#else`). Generated under an alphabet that
    /// excludes `"`, `(`, `` ` ``, `'`, `#`, `\r`, `\n` inside content so we
    /// never produce multi-line tokens or stray directive-like lines.
    #[derive(Clone, Debug)]
    enum Block {
        Content(String),
        /// A `#line` directive line. `bare` selects the `# N "f"`
        /// alternate over `#line N "f"`; both are newline-terminated by
        /// `render`, so both parse to `Directive::Line`.
        Line {
            number: u32,
            file: Option<String>,
            bare: bool,
        },
        /// A `#nowarn` / `#warnon` directive line (`on` selects `#warnon`).
        Warn {
            on: bool,
        },
        If {
            ident: String,
            then_blocks: Vec<Block>,
            else_blocks: Option<Vec<Block>>,
        },
    }

    fn arb_content() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\t'),
                Just('a'),
                Just('b'),
                Just('x'),
                Just('y'),
                Just('1'),
                Just('2'),
                Just('+'),
                Just('='),
                Just(';'),
                Just(','),
                Just('-'),
                Just('.'),
                Just('_'),
            ],
            0..12,
        )
        .prop_map(|cs| cs.into_iter().collect())
    }

    fn arb_ident() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("A".to_string()),
            Just("B".to_string()),
            Just("C".to_string()),
            Just("D".to_string()),
        ]
    }

    fn arb_line_block() -> impl Strategy<Value = Block> {
        (
            0u32..1000,
            prop::option::of(prop_oneof![
                Just("foo.fsl".to_string()),
                Just("bar.fsy".to_string()),
            ]),
            any::<bool>(),
        )
            .prop_map(|(number, file, bare)| Block::Line { number, file, bare })
    }

    fn arb_warn_block() -> impl Strategy<Value = Block> {
        any::<bool>().prop_map(|on| Block::Warn { on })
    }

    fn arb_block() -> impl Strategy<Value = Block> {
        let leaf = arb_content().prop_map(Block::Content);
        leaf.prop_recursive(3, 16, 3, |inner| {
            prop_oneof![
                arb_content().prop_map(Block::Content),
                arb_line_block(),
                arb_warn_block(),
                (
                    arb_ident(),
                    prop::collection::vec(inner.clone(), 0..3),
                    prop::option::of(prop::collection::vec(inner, 0..3)),
                )
                    .prop_map(|(ident, then_blocks, else_blocks)| Block::If {
                        ident,
                        then_blocks,
                        else_blocks,
                    }),
            ]
        })
    }

    fn render(blocks: &[Block], out: &mut String) {
        for b in blocks {
            match b {
                Block::Content(s) => {
                    out.push_str(s);
                    out.push('\n');
                }
                Block::Line { number, file, bare } => {
                    out.push_str(if *bare { "# " } else { "#line " });
                    out.push_str(&number.to_string());
                    if let Some(f) = file {
                        out.push_str(" \"");
                        out.push_str(f);
                        out.push('"');
                    }
                    out.push('\n');
                }
                Block::Warn { on } => {
                    out.push_str(if *on { "#warnon " } else { "#nowarn " });
                    out.push_str("\"40\"");
                    out.push('\n');
                }
                Block::If {
                    ident,
                    then_blocks,
                    else_blocks,
                } => {
                    out.push_str("#if ");
                    out.push_str(ident);
                    out.push('\n');
                    render(then_blocks, out);
                    if let Some(eb) = else_blocks {
                        out.push_str("#else\n");
                        render(eb, out);
                    }
                    out.push_str("#endif\n");
                }
            }
        }
    }

    fn arb_program() -> impl Strategy<Value = String> {
        prop::collection::vec(arb_block(), 0..4).prop_map(|blocks| {
            let mut s = String::new();
            render(&blocks, &mut s);
            s
        })
    }

    fn arb_symbols() -> impl Strategy<Value = HashSet<String>> {
        prop::collection::hash_set(arb_ident(), 0..5)
    }

    proptest! {
        #[test]
        fn driver_is_total(s in arb_program(), symbols in arb_symbols()) {
            // Just consume the iterator and make sure it terminates.
            let _: Vec<_> = lex_with_symbols(&s, &symbols).collect();
        }

        /// Full-trivia mode never panics either.
        #[test]
        fn full_trivia_is_total(s in arb_program(), symbols in arb_symbols()) {
            let _: Vec<_> = lex_with_symbols_full_trivia(&s, &symbols).collect();
        }

        /// Additive-equivalence: full-trivia mode only *inserts* directive
        /// trivia. Dropping the `HashLine` / `WarnDirective` markers and
        /// unwrapping `Lexed` recovers the swallow-mode stream exactly —
        /// token-for-token, span-for-span, error-for-error.
        #[test]
        fn full_trivia_minus_directives_equals_swallow(
            s in arb_program(),
            symbols in arb_symbols(),
        ) {
            let full: Vec<_> = lex_with_symbols_full_trivia(&s, &symbols)
                .filter_map(|(res, span)| match res {
                    Ok(TriviaToken::Lexed(t)) => Some((Ok(t), span)),
                    // Every non-`Lexed` marker is pure trivia — drop it.
                    Ok(_) => None,
                    Err(e) => Some((Err(e), span)),
                })
                .collect();
            let swallow: Vec<_> = lex_with_symbols(&s, &symbols).collect();
            prop_assert_eq!(full, swallow);
        }

        /// The emitted *directive* trivia tokens (kind + span), in order,
        /// equal an independent reference walk over the visible directives
        /// (active-branch `#line` / `#nowarn` / `#warnon` and parent-active
        /// `#if` / `#else` / `#elif` / `#endif`). `INACTIVECODE` is excluded
        /// here — the dead-region partition is covered by
        /// `full_trivia_tokens_partition_source` /
        /// `inactive_code_covers_only_dead_bytes`.
        #[test]
        fn full_trivia_directive_spans_match_reference(
            s in arb_program(),
            symbols in arb_symbols(),
        ) {
            let fast: Vec<(SyntaxKind, Range<usize>)> =
                lex_with_symbols_full_trivia(&s, &symbols)
                    .filter_map(|(res, span)| match res {
                        Ok(tt) => match tt.trivia_syntax_kind() {
                            Some(SyntaxKind::INACTIVECODE) | None => None,
                            Some(k) => Some((k, span)),
                        },
                        Err(_) => None,
                    })
                    .collect();
            let reference = reference::collect_trivia_directives_reference(&s, &symbols);
            prop_assert_eq!(fast, reference);
        }

        /// Byte-completeness: in full-trivia mode the emitted token spans
        /// tile `[0, source.len())` with no gaps or overlaps, and their texts
        /// concatenate back to the source. Balanced sources produce no `Err`
        /// items (see `no_errors_on_balanced_sources`), so the `Ok` spans are
        /// the whole partition.
        #[test]
        fn full_trivia_tokens_partition_source(
            s in arb_program(),
            symbols in arb_symbols(),
        ) {
            let spans: Vec<Range<usize>> = lex_with_symbols_full_trivia(&s, &symbols)
                .filter_map(|(res, span)| res.is_ok().then_some(span))
                .collect();
            let mut end = 0usize;
            for span in &spans {
                prop_assert_eq!(span.start, end, "gap/overlap before {:?}", span);
                end = span.end;
            }
            prop_assert_eq!(end, s.len(), "tokens do not reach EOF");
            let concat: String = spans.iter().map(|sp| &s[sp.clone()]).collect();
            prop_assert_eq!(concat, s);
        }

        /// Every `INACTIVECODE` token covers only inactive (dead-branch)
        /// bytes — it never overlaps active code (which must lex) or a
        /// visible directive line.
        #[test]
        fn inactive_code_covers_only_dead_bytes(
            s in arb_program(),
            symbols in arb_symbols(),
        ) {
            let active = reference::active_mask(&s, &symbols);
            for (res, span) in lex_with_symbols_full_trivia(&s, &symbols) {
                if matches!(res, Ok(TriviaToken::InactiveCode)) {
                    for i in span.clone() {
                        prop_assert!(!active[i], "INACTIVECODE over active byte {} in {:?}", i, span);
                    }
                }
            }
        }

        #[test]
        fn fast_matches_reference_on_balanced_sources(
            s in arb_program(),
            symbols in arb_symbols(),
        ) {
            let fast = ok_tokens(lex_with_symbols(&s, &symbols));
            let reference = ref_tokens(reference::reference_lex(&s, &symbols).into_iter());
            prop_assert_eq!(fast, reference);
        }

        /// No errors should be produced on balanced sources.
        #[test]
        fn no_errors_on_balanced_sources(
            s in arb_program(),
            symbols in arb_symbols(),
        ) {
            let errs: Vec<_> = lex_with_symbols(&s, &symbols)
                .filter_map(|(t, _)| t.err())
                .collect();
            prop_assert!(errs.is_empty(), "errors on balanced source {:?}: {:?}", s, errs);
        }

        /// The driver's captured `#line` store matches the independent
        /// reference collector on balanced sources.
        #[test]
        fn line_directives_match_reference(
            s in arb_program(),
            symbols in arb_symbols(),
        ) {
            let fast = collect_store(&s, &symbols);
            let reference = reference::collect_line_directives_reference(&s, &symbols);
            prop_assert_eq!(fast, reference);
        }

        /// Captured directives are ascending by `generated_line` — the
        /// ordering invariant the future remap query depends on.
        #[test]
        fn captured_generated_lines_are_ascending(
            s in arb_program(),
            symbols in arb_symbols(),
        ) {
            let store = collect_store(&s, &symbols);
            let lines: Vec<u32> = store.directives().iter().map(|d| d.generated_line).collect();
            let mut sorted = lines.clone();
            sorted.sort_unstable();
            prop_assert_eq!(lines, sorted);
        }
    }
}
