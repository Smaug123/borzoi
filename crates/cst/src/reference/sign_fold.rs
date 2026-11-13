//! The parser's sign-folding pass (`parser::sign_fold`) as it stood before
//! the token tables, frozen as the reference for the filtered stream
//! (`docs/token-pipeline-soa-plan.md`, D7). Verbatim but for these imports and
//! the dropped unit tests.

use crate::directives::{PreprocError, TriviaToken};
use crate::lexer::{LexError, Span, Token};
use std::ops::Range;

use super::lexfilter::{FilteredToken, is_atomic_expr_end};

type RawTok<'src> = (Result<TriviaToken<'src>, PreprocError>, Range<usize>);
type FilteredTok<'src> = (Result<FilteredToken<'src>, LexError>, Range<usize>);

/// The significant (non-trivia) real token a raw entry carries.
fn raw_significant<'a, 'src>(tt: &'a TriviaToken<'src>) -> Option<&'a Token<'src>> {
    match tt {
        TriviaToken::Lexed(t)
            if !matches!(
                t,
                Token::Whitespace | Token::Newline | Token::LineComment | Token::BlockComment
            ) =>
        {
            Some(t)
        }
        _ => None,
    }
}

/// If `lit` is a numeric literal FCS folds a sign into, rebuild it carrying
/// `merged` (the source slice spanning the sign *and* the digits) as its
/// text. Returns `None` for non-foldable tokens — unsigned suffixed ints
/// (suffix contains `u`/`U`) and everything that isn't a foldable numeric
/// literal.
fn rebuild_signed_literal<'src>(lit: &Token<'src>, merged: &'src str) -> Option<Token<'src>> {
    match lit {
        Token::Int(_) => Some(Token::Int(merged)),
        Token::XInt(_) => Some(Token::XInt(merged)),
        Token::Float64(_) => Some(Token::Float64(merged)),
        Token::Float32(_) => Some(Token::Float32(merged)),
        Token::XIEEE64(_) => Some(Token::XIEEE64(merged)),
        Token::XIEEE32(_) => Some(Token::XIEEE32(merged)),
        Token::Decimal(_) => Some(Token::Decimal(merged)),
        Token::BigNum(_) => Some(Token::BigNum(merged)),
        // Suffixed ints fold only when the suffix is *signed*. The suffix
        // set is `{y,u,s,l,n,L,U}`; an unsigned form is exactly one
        // containing `u`/`U` (`uy`/`us`/`u`/`ul`/`un`/`uL`/`UL`). Hex/oct/bin
        // prefixes (`0x`/`0o`/`0b`) and digits never contain `u`/`U`, so a
        // whole-text check is unambiguous.
        Token::IntSuffixed(s) if !s.contains(['u', 'U']) => Some(Token::IntSuffixed(merged)),
        Token::XIntSuffixed(s) if !s.contains(['u', 'U']) => Some(Token::XIntSuffixed(merged)),
        _ => None,
    }
}

/// FCS's `prevWasAtomicEnd && lastTokenPos == startOfThisToken`: `true` when
/// the token immediately to the left of the op (in the **raw** stream, which
/// still carries any LexFilter-swallowed `)`/`}`) is an atomic-expr-end token
/// glued to the op's start. When `true`, the op is infix and must *not* fold.
fn glued_to_atomic_end(raw_tokens: &[RawTok<'_>], op_span: &Span) -> bool {
    // `raw_tokens` is position-sorted and non-overlapping, so binary-search to
    // the first token ending *after* the op rather than scanning from the end —
    // keeping the whole fold pass ~linear even in files dense with signed
    // literals (`[| -1; -2; … |]`). The predecessors are then the short
    // trailing trivia run before `pp`, walked backward to the first
    // significant token.
    let pp = raw_tokens.partition_point(|(_, s)| s.end <= op_span.start);
    let prev = raw_tokens[..pp]
        .iter()
        .rev()
        .find_map(|(res, s)| match res {
            Ok(tt) => raw_significant(tt).map(|t| (t, s.end)),
            Err(_) => None,
        });
    match prev {
        Some((tok, end)) => end == op_span.start && is_atomic_expr_end(tok),
        None => false,
    }
}

/// Try to fold the op at filtered index `i` with the literal at `i + 1`.
/// Returns the merged filtered token on success.
fn try_fold<'src>(
    source: &'src str,
    raw_tokens: &[RawTok<'src>],
    filtered: &[FilteredTok<'src>],
    i: usize,
) -> Option<FilteredTok<'src>> {
    let (op_res, op_span) = filtered.get(i)?;
    let Ok(FilteredToken::Raw(Token::Op(op_text))) = op_res else {
        return None;
    };
    if *op_text != "-" && *op_text != "+" {
        return None;
    }

    let (lit_res, lit_span) = filtered.get(i + 1)?;
    let Ok(FilteredToken::Raw(lit_tok)) = lit_res else {
        return None;
    };
    // Adjacent-right: the literal must begin exactly where the op ends.
    if lit_span.start != op_span.end {
        return None;
    }
    let merged_text = &source[op_span.start..lit_span.end];
    let merged_tok = rebuild_signed_literal(lit_tok, merged_text)?;

    // Non-adjacent-left, against the raw stream.
    if glued_to_atomic_end(raw_tokens, op_span) {
        return None;
    }

    Some((
        Ok(FilteredToken::Raw(merged_tok)),
        op_span.start..lit_span.end,
    ))
}

/// Fold every adjacent `±literal` pair in `filtered` into a single signed
/// literal token, mirroring FCS's token-layer sign fold. The merged token's
/// span covers the sign and the digits (contiguous, since adjacency forbids
/// intervening trivia), so the dual-stream bump
/// (`Parser::bump_into`) reclaims both underlying raw tokens and the
/// tree stays lossless. Non-foldable `+`/`-` (spaced, glued-left, or a
/// non-literal/unsigned operand) are left untouched for the parser's existing
/// prefix/infix/`ADJACENT_PREFIX_OP` handling.
pub fn fold_adjacent_signs<'src>(
    source: &'src str,
    raw_tokens: &[RawTok<'src>],
    mut filtered: Vec<FilteredTok<'src>>,
) -> Vec<FilteredTok<'src>> {
    // In-place left-compaction: a fold merges two tokens into one, so the
    // output is never longer than the input. Rewrite the Vec we already own —
    // a `write` cursor trailing a `read` cursor — instead of cloning the whole
    // stream into a fresh allocation. `write <= read` always, so `[write, read)`
    // only ever holds already-consumed entries; overwriting them loses nothing,
    // and `try_fold`'s immutable borrow ends before each mutation.
    let mut read = 0;
    let mut write = 0;
    while read < filtered.len() {
        if let Some(merged) = try_fold(source, raw_tokens, &filtered, read) {
            filtered[write] = merged;
            read += 2;
        } else {
            if write != read {
                filtered.swap(write, read);
            }
            read += 1;
        }
        write += 1;
    }
    filtered.truncate(write);
    filtered
}
