//! Interpolated-string state-machine wrapper around the raw Logos stream.
//!
//! The two Logos arms for `$"` and `$"""` (see [`super::Token::InterpString`])
//! recognise the four opener shapes that fit a regex: bare single-quoted
//! `$"hello"`, single-quoted with a fill `$"hello {`, bare triple-quoted
//! `$"""hello"""`, and triple-quoted with a fill `$"""hello {` (each opener
//! span includes the trailing `{` when present). After a `Begin` /
//! `TripleBegin`, the next bytes are an F# expression (the fill body) —
//! Logos can tokenise those normally, but the matching `}` that closes the
//! fill must be intercepted: instead of emitting `RBrace`, the wrapper
//! byte-walks the next string fragment up to the next unescaped `{` or
//! string closer (`"` for single-quoted, `"""` for triple-quoted), and
//! synthesises [`InterpKind::Part`] (the fragment ends with `{`, more fills
//! follow) or [`InterpKind::End`] (the fragment ends with the closer, fill
//! chain over). The continuation tokens are style-agnostic — the driver's
//! per-frame `InterpStyle` picks the closer and escape rules. Logos
//! resumes from the byte after that.
//!
//! Nesting: a fill's expression may legitimately contain its own balanced
//! `{ ... }`, e.g. `$"x={ {| f = 1 |} }"`. Plain `LBrace`/`RBrace` are
//! depth-counted within a frame; only the `RBrace` at depth 0 terminates the
//! fill. The compound tokens `LBraceBar` (`{|`) and `BarRBrace` (`|}`) do
//! *not* affect the depth — they're single Logos tokens whose lexer-level
//! split is irrelevant for interp balancing.
//!
//! State is a [`Vec`] of frames so nested interp strings — `$"outer {$"inner"}
//! more"` — work naturally (one frame per active opener). The single-fill
//! case touches the stack with exactly one frame; the bare `BeginEnd` case
//! never pushes.
//!
//! # Cross-boundary callers
//!
//! The directive driver ([`crate::directives::driver`]) invalidates and
//! re-creates the inner lexer at each `#if`/`#else`/`#endif` boundary. A
//! naive recreation would start a fresh [`InterpDriver`] with an empty
//! frame stack, mis-tokenising the closing `}` of a fill straddling the
//! directive as a plain `RBrace`. The crate-private `snapshot_frames` +
//! `new_with_frames` pair on [`InterpDriver`] hands the frame stack
//! across the invalidation so directive lines inside a fill
//! (`$"{\n#if FOO\n1\n#endif\n}"`) keep the fill alive.

use std::ops::Range;

use logos::{Logos, SpannedIter};

use super::{InterpKind, LexError, Span, Token, callbacks::single_quote_escape_len};

/// Delimiter shape of the enclosing interpolated string. The byte-walker
/// in [`InterpDriver::scan_cont`] dispatches on this to pick the right
/// closer and escape rules for the next fragment after a depth-0 `}`.
///
/// * [`InterpStyle::Single`] — `$"..."`: recognised `singleQuoteString`
///   backslash escapes are content, but unrecognised pairs such as `\{` do not
///   hide interpolation delimiters. `{{`/`}}` are literal-brace escapes,
///   closer is `"`, newlines in the body are an error in FCS (we accept them
///   and let parser recovery sort it out).
/// * [`InterpStyle::Triple`] — `$"""..."""`: no backslash escape (`\`
///   is a literal byte), `{{`/`}}` are literal-brace escapes, closer
///   is `"""` (greedy first run of three or more `"`), newlines are
///   content.
/// * [`InterpStyle::Verbatim`] — `$@"..."` / `@$"..."`: no backslash
///   escape (`\` is a literal byte), `""` is a literal quote (does *not*
///   terminate), `{{`/`}}` are literal-brace escapes, closer is a single
///   `"` (not doubled), newlines are content.
/// * [`InterpStyle::Extended`] — `$$"""..."""` (≥2 `$`): triple-like, but
///   the fill delimiter is `n` braces (`n` = leading `$` count). No
///   backslash escape, no `{{`/`}}` digraph, closer is `"""`, newlines are
///   content. A `{`-run ≥ `n` opens a fill, a `}`-run of `n` closes one; a
///   run shorter than `n` is content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterpStyle {
    Single,
    Triple,
    Verbatim,
    Extended { n: usize },
}

/// One active interpolated-string opener. Each entry corresponds to a
/// [`InterpKind::Begin`] or [`InterpKind::TripleBegin`] that hasn't yet
/// been matched by its [`InterpKind::End`]. `brace_depth` counts plain
/// `{`/`}` seen inside the current fill of this frame; the frame's
/// fill closes when a `}` arrives at depth 0. `style` chooses the
/// byte-walker's escape rules and terminator.
///
/// The directive driver snapshots and restores this state across lexer
/// recreations via [`InterpDriver::snapshot_frames`] /
/// [`InterpDriver::new_with_frames`]; the type is `pub(crate)` so it can
/// flow through that path but its contents stay opaque to callers.
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    /// Plain-brace nesting inside the current fill. `{` increments, `}`
    /// decrements; the depth-0 `}` is what terminates the fill.
    brace_depth: u32,
    /// Delimiter style of the enclosing string — see [`InterpStyle`].
    style: InterpStyle,
}

/// State machine wrapping a [`SpannedIter`]. See module docs.
pub struct InterpDriver<'a> {
    source: &'a str,
    /// Logos lexer over `source[base..]`. Recreated whenever the driver
    /// byte-walks past a position the inner iterator didn't see (i.e. when
    /// emitting `Part`/`End` after intercepting a depth-0 `}`).
    inner: SpannedIter<'a, Token<'a>>,
    /// Absolute byte offset where `inner` was constructed. Inner spans are
    /// relative; add `base` to get absolute.
    base: usize,
    /// Stack of active interp frames. Empty ⇒ not inside any fill ⇒
    /// transparent pass-through over `inner`.
    frames: Vec<Frame>,
}

impl<'a> InterpDriver<'a> {
    pub fn new(source: &'a str) -> Self {
        Self::new_with_frames(source, Vec::new())
    }

    /// Construct an `InterpDriver` over `source` with a pre-populated frame
    /// stack. The directive driver uses this to thread an active fill
    /// through a `#if`/`#endif` lexer recreation: snapshot the frames
    /// before invalidation, hand them back here on the rebuild so the
    /// closing `}` of a straddling fill is still recognised as
    /// [`InterpKind::End`] rather than a stray `RBrace`.
    pub(crate) fn new_with_frames(source: &'a str, frames: Vec<Frame>) -> Self {
        Self {
            source,
            inner: Token::lexer(source).spanned(),
            base: 0,
            frames,
        }
    }

    /// Clone the current frame stack. See [`Self::new_with_frames`].
    pub(crate) fn snapshot_frames(&self) -> Vec<Frame> {
        self.frames.clone()
    }

    /// Restart the inner Logos lexer at `pos`. Used after the driver
    /// byte-walks past `}...{` or `}..."` — Logos was positioned where the
    /// `}` was about to be consumed, but we've now jumped past the next
    /// string fragment.
    fn restart_inner(&mut self, pos: usize) {
        self.inner = Token::lexer(&self.source[pos..]).spanned();
        self.base = pos;
    }

    /// Byte-walk a continuation fragment from `start` (which points at
    /// the terminating `}` of the just-closed fill). Returns the
    /// [`InterpKind`] (`Part` if a `{` came first, `End` if a `"` —
    /// resp. `"""` for triple — came first; spans inclusive of the
    /// bracketing chars) and the end byte. `style` chooses escape rules
    /// and terminator; see [`InterpStyle`].
    ///
    /// Returns `(End, source.len())` and a paired
    /// [`LexError::UnterminatedString`] if EOF is reached without a
    /// closer.
    fn scan_cont(&self, start: usize, style: InterpStyle) -> (Result<InterpKind, LexError>, usize) {
        let bytes = self.source.as_bytes();
        debug_assert_eq!(bytes[start], b'}');
        if let InterpStyle::Extended { n } = style {
            return self.scan_cont_extended(start, n);
        }
        let mut i = start + 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if style == InterpStyle::Single => {
                    if let Some(len) = single_quote_escape_len(bytes, i) {
                        i += len;
                    } else {
                        i += 1;
                    }
                }
                b'{' => {
                    if bytes.get(i + 1) == Some(&b'{') {
                        i += 2;
                        continue;
                    }
                    return (Ok(InterpKind::Part), i + 1);
                }
                b'}' => {
                    if bytes.get(i + 1) == Some(&b'}') {
                        i += 2;
                        continue;
                    }
                    // Stray `}` mid-fragment — same as the opener: keep
                    // walking, defer diagnosis to a later phase.
                    i += 1;
                }
                b'"' => match style {
                    InterpStyle::Single => {
                        let is_byte = bytes.get(i + 1) == Some(&b'B');
                        return (
                            Ok(InterpKind::End { is_byte }),
                            i + 1 + usize::from(is_byte),
                        );
                    }
                    InterpStyle::Verbatim => {
                        if bytes.get(i + 1) == Some(&b'"') {
                            // `""` is a literal quote in a verbatim body.
                            i += 2;
                            continue;
                        }
                        let is_byte = bytes.get(i + 1) == Some(&b'B');
                        return (
                            Ok(InterpKind::End { is_byte }),
                            i + 1 + usize::from(is_byte),
                        );
                    }
                    InterpStyle::Triple => {
                        if bytes.get(i + 1) == Some(&b'"') && bytes.get(i + 2) == Some(&b'"') {
                            let is_byte = bytes.get(i + 3) == Some(&b'B');
                            return (
                                Ok(InterpKind::End { is_byte }),
                                i + 3 + usize::from(is_byte),
                            );
                        }
                        // One or two `"` in a triple-quoted body are
                        // content; advance and keep scanning.
                        i += 1;
                    }
                    InterpStyle::Extended { .. } => {
                        unreachable!("extended style is dispatched to scan_cont_extended")
                    }
                },
                _ => i += 1,
            }
        }
        (Err(LexError::UnterminatedString), bytes.len())
    }

    /// Extended (`$$"""…"""`) variant of [`Self::scan_cont`]. `start` points
    /// at the first `}` of the just-closed fill; `n` is the delimiter length.
    /// Skips the `n`-brace close delimiter, then byte-walks triple-like
    /// content (no backslash, no `{{`/`}}` digraph) until the next fill-open
    /// (`{`-run ≥ `n` → [`InterpKind::Part`], whole run consumed) or the
    /// closing `"""` ([`InterpKind::End`]; extended has no byte suffix). A
    /// `{`/`}` run shorter than `n` is content; a stray `}`-run is content
    /// here (the parser diagnoses an over-long one as FS1249).
    fn scan_cont_extended(&self, start: usize, n: usize) -> (Result<InterpKind, LexError>, usize) {
        let bytes = self.source.as_bytes();
        // Skip the `n`-brace close delimiter. The caller only dispatches here
        // when the `}`-run at `start` is ≥ `n` (a shorter run does not close),
        // so `close_run.min(n)` is exactly `n`; any extra `}` past the first
        // `n` are string-body content scanned below.
        let close_run = bytes[start..].iter().take_while(|&&b| b == b'}').count();
        let mut i = start + close_run.min(n);
        while i < bytes.len() {
            match bytes[i] {
                b'{' => {
                    let run = bytes[i..].iter().take_while(|&&b| b == b'{').count();
                    if run >= n {
                        return (Ok(InterpKind::Part), i + run);
                    }
                    i += run;
                }
                b'"' if bytes.get(i + 1) == Some(&b'"') && bytes.get(i + 2) == Some(&b'"') => {
                    return (Ok(InterpKind::End { is_byte: false }), i + 3);
                }
                _ => i += 1,
            }
        }
        (Err(LexError::UnterminatedString), bytes.len())
    }
}

impl<'a> Iterator for InterpDriver<'a> {
    type Item = (Result<Token<'a>, LexError>, Span);

    fn next(&mut self) -> Option<Self::Item> {
        let (tok, rel_span) = self.inner.next()?;
        let abs: Range<usize> = (rel_span.start + self.base)..(rel_span.end + self.base);

        match tok {
            Ok(Token::InterpString(InterpKind::Begin)) => {
                self.frames.push(Frame {
                    brace_depth: 0,
                    style: InterpStyle::Single,
                });
                Some((Ok(Token::InterpString(InterpKind::Begin)), abs))
            }
            Ok(Token::InterpString(InterpKind::TripleBegin)) => {
                self.frames.push(Frame {
                    brace_depth: 0,
                    style: InterpStyle::Triple,
                });
                Some((Ok(Token::InterpString(InterpKind::TripleBegin)), abs))
            }
            Ok(Token::InterpString(InterpKind::VerbatimBegin)) => {
                self.frames.push(Frame {
                    brace_depth: 0,
                    style: InterpStyle::Verbatim,
                });
                Some((Ok(Token::InterpString(InterpKind::VerbatimBegin)), abs))
            }
            Ok(Token::InterpString(InterpKind::ExtendedBegin { n })) => {
                self.frames.push(Frame {
                    brace_depth: 0,
                    style: InterpStyle::Extended { n },
                });
                Some((
                    Ok(Token::InterpString(InterpKind::ExtendedBegin { n })),
                    abs,
                ))
            }
            Ok(Token::LBrace) if !self.frames.is_empty() => {
                let top = self.frames.last_mut().expect("frame");
                top.brace_depth += 1;
                Some((Ok(Token::LBrace), abs))
            }
            Ok(Token::RBrace) if !self.frames.is_empty() => {
                let top = self.frames.last_mut().expect("frame");
                if top.brace_depth > 0 {
                    top.brace_depth -= 1;
                    Some((Ok(Token::RBrace), abs))
                } else {
                    // Depth-0 `}` — normally this closes the active fill.
                    let style = top.style;
                    let close_at = abs.start;
                    // Extended (`$$"""…"""`, N>1) closes only on a `}`-run of
                    // ≥ N at brace-counter top level; a shorter run is an
                    // ordinary `RBrace` token in the fill expression (FCS keeps
                    // the interpolation open). Emit it and stay in the frame.
                    if let InterpStyle::Extended { n } = style {
                        let run = self.source.as_bytes()[close_at..]
                            .iter()
                            .take_while(|&&b| b == b'}')
                            .count();
                        if run < n {
                            return Some((Ok(Token::RBrace), abs));
                        }
                    }
                    // Swallow the closer, then byte-walk the next string
                    // fragment using the active frame's style.
                    let (kind, end_abs) = self.scan_cont(close_at, style);
                    let span = close_at..end_abs;
                    let popped = matches!(kind, Ok(InterpKind::End { .. }) | Err(_));
                    if popped {
                        self.frames.pop();
                    }
                    self.restart_inner(end_abs);
                    Some((kind.map(Token::InterpString), span))
                }
            }
            other => Some((other, abs)),
        }
    }
}
