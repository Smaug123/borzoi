//! Adjacent sign-folding — a parser-input pass that merges an adjacent
//! `+`/`-` into the following numeric-literal token, mirroring FCS's
//! `LexFilter.fs:2694` (`hwTokenFetch`).
//!
//! FCS folds the sign at the token layer, *before* the grammar runs, so a
//! folded literal is a single token by the time the parser sees it. We
//! reproduce that here as a pass over the parser's filtered-token vector
//! (run in [`super::parse_with_symbols`] after `lexfilter::filter`, before
//! [`super::Parser`] consumes it). Doing it at the token layer means **both**
//! expression and pattern positions — and arg position (`f -1`), paren bodies
//! (`(-1)`), etc. — get a single signed literal token with **zero** grammar
//! changes: the const-payload dispatch ([`super::Parser::parse_const_payload`])
//! and the pattern atom path both already accept the literal kinds.
//!
//! # What folds
//!
//! FCS folds an adjacent `+`/`-` into `INT8`/`INT16`/`INT32`/`INT64`/
//! `NATIVEINT`/`IEEE32`/`IEEE64`/`DECIMAL`/`BIGNUM`
//! (`LexFilter.fs:2737-2748`). We mirror that for the corresponding lexer
//! tokens, including hex-bit-pattern floats (`XIEEE32`/`XIEEE64`): the
//! normaliser decodes the bit pattern then flips the IEEE sign bit for `-`.
//! Unsigned suffixed ints (`-1uy`) are intentionally excluded because they are
//! not in FCS's fold set.
//!
//! # When it folds
//!
//! FCS's guard is `nextTokenIsAdjacent && not (prevWasAtomicEnd &&
//! lastTokenPos == startOfThisToken)`:
//! - **adjacent-right** — the literal begins exactly where the op ends (no
//!   intervening trivia). Checked by byte span against the next filtered
//!   token. So `-1` folds but spaced `- 1` does not.
//! - **non-adjacent-left** — the op is *not* glued to the right edge of a
//!   preceding atomic-expression-end token. Computed against the **raw**
//!   stream, because a LexFilter-swallowed `)`/`}` is gone from the filtered
//!   stream (so `(x)-1` would otherwise look non-adjacent-left and wrongly
//!   fold). So `f-1`/`x-1` stay infix, but `f -1`, `1 +2`, `(-1)`,
//!   `return -1` fold.

use super::*;
use crate::lexer::Span;
// FCS's `isAtomicExprEndToken` lives in the lex-filter (its FCS home,
// `LexFilter.fs:394`); the offside `ADJACENT_PREFIX_OP` rule and this sign-fold
// pass share the one definition.
use crate::lexfilter::is_atomic_expr_end;

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
/// ([`super::Parser::bump_into`]) reclaims both underlying raw tokens and the
/// green tree stays lossless. Non-foldable `+`/`-` (spaced, glued-left, or a
/// non-literal/unsigned operand) are left untouched for the parser's existing
/// prefix/infix/`ADJACENT_PREFIX_OP` handling.
pub(super) fn fold_adjacent_signs<'src>(
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

#[cfg(test)]
mod tests {
    use crate::parser::parse;

    /// Render the green tree so a test can assert the presence/absence of a
    /// folded literal token (`INT32_LIT "-1"`) without standing up the FCS
    /// oracle — these pin the adjacency logic directly and run everywhere.
    fn tree(src: &str) -> String {
        format!("{:#?}", parse(src).root)
    }

    #[test]
    fn adjacent_minus_before_literal_folds() {
        let t = tree("-1\n");
        assert!(
            t.contains("INT32_LIT@0..2 \"-1\""),
            "expected folded `-1`:\n{t}"
        );
        // No prefix-op application node survives.
        assert!(!t.contains("APP_EXPR"), "`-1` must not be an App:\n{t}");
    }

    #[test]
    fn adjacent_plus_before_literal_folds() {
        let t = tree("+1\n");
        assert!(
            t.contains("INT32_LIT@0..2 \"+1\""),
            "expected folded `+1`:\n{t}"
        );
    }

    #[test]
    fn spaced_minus_does_not_fold() {
        // `- 1` keeps the prefix op — folding is adjacent-only.
        let t = tree("- 1\n");
        assert!(t.contains("APP_EXPR"), "`- 1` must stay `App(~-, 1)`:\n{t}");
        assert!(
            !t.contains("\"- 1\"") && !t.contains("INT32_LIT@0..3"),
            "`- 1` must not merge across the space:\n{t}"
        );
    }

    #[test]
    fn minus_glued_to_atomic_end_does_not_fold() {
        // `f-1` — `-` is glued to the atomic-end ident `f`, so it stays infix
        // subtraction, not a folded literal.
        let t = tree("f-1\n");
        assert!(
            t.contains("INFIX_APP_EXPR"),
            "`f-1` must stay infix subtraction:\n{t}"
        );
    }

    #[test]
    fn minus_first_in_parens_folds() {
        // `(-1)` — no atomic-end token to the left of `-`, so it folds.
        let t = tree("(-1)\n");
        assert!(
            t.contains("INT32_LIT@1..3 \"-1\""),
            "`(-1)` should fold inside the parens:\n{t}"
        );
    }

    #[test]
    fn unsigned_suffix_does_not_fold() {
        // `-1uy` — unsigned byte isn't in FCS's fold set, so `-` stays a prefix.
        let t = tree("-1uy\n");
        assert!(
            t.contains("APP_EXPR"),
            "`-1uy` must stay `App(~-, 1uy)` (unsigned excluded):\n{t}"
        );
        assert!(
            !t.contains("\"-1uy\""),
            "`-1uy` must not merge the sign into the literal:\n{t}"
        );
    }

    #[test]
    fn fold_applies_in_pattern_position() {
        // The fold is a token-layer pass, so negative-literal *patterns* parse
        // (pre-fold this cascaded into errors).
        let parse = parse("match x with\n| -1 -> a\n| _ -> b\n");
        let t = format!("{:#?}", parse.root);
        assert!(
            t.contains("CONST_PAT") && t.contains("INT32_LIT@15..17 \"-1\""),
            "`-1` should be a folded const pattern:\n{t}"
        );
        assert!(
            parse.errors.is_empty(),
            "negative-literal pattern must parse cleanly, got: {:?}",
            parse.errors
        );
    }

    #[test]
    fn int32_min_folds_without_error() {
        // `-2147483648` is valid; pre-fold we emitted a spurious range error.
        let parse = parse("-2147483648\n");
        let t = format!("{:#?}", parse.root);
        assert!(
            t.contains("INT32_LIT@0..11 \"-2147483648\""),
            "`-2147483648` should fold to one literal token:\n{t}"
        );
        assert!(
            parse.errors.is_empty(),
            "`-2147483648` (i32::MIN) must not error, got: {:?}",
            parse.errors
        );
    }

    #[test]
    fn bare_int32_overflow_still_errors() {
        // The fold must not mask a genuine overflow: bare `2147483648` still
        // errors (only a folded `-` rescues `MaxValue + 1`).
        let parse = parse("2147483648\n");
        assert!(
            !parse.errors.is_empty(),
            "bare `2147483648` (i32::MAX + 1) must still error"
        );
    }

    #[test]
    fn plus_overflow_still_errors() {
        // A folded `+` does NOT clear the overflow (`plus && bad` keeps `bad`).
        let parse = parse("+2147483648\n");
        assert!(
            !parse.errors.is_empty(),
            "`+2147483648` must still error (only `-` rescues MaxValue+1)"
        );
    }

    #[test]
    fn leading_zero_int32_min_still_errors() {
        // FCS's `isInt32BadMax` rescues only the exact cleaned string
        // `"2147483648"`; a leading-zero spelling overflows even under `-`.
        for src in ["-02147483648\n", "-02147483648l\n"] {
            let parse = parse(src);
            assert!(
                !parse.errors.is_empty(),
                "{src:?} is a leading-zero int32 MaxValue+1 — FCS still errors"
            );
        }
    }

    #[test]
    fn leading_zero_int64_min_still_errors() {
        // Same `isInt64BadMax` exact-string rescue for int64 / nativeint.
        for src in ["-09223372036854775808L\n", "-09223372036854775808n\n"] {
            let parse = parse(src);
            assert!(
                !parse.errors.is_empty(),
                "{src:?} is a leading-zero int64 MaxValue+1 — FCS still errors"
            );
        }
    }

    #[test]
    fn fold_in_pattern_continuation_positions() {
        // The pattern-start gates consult the raw stream (for the swallowed-`)`
        // guard), which still shows the pre-fold `Op("-")`. These continuation/
        // nested positions must still recognise the folded constant. (codex
        // round-2 P2.)
        for src in [
            "match x with\n| Some -1 -> a\n| _ -> b\n", // constructor arg
            "match x with\n| 1, -1 -> a\n| _ -> b\n",   // tuple element after `,`
            "match x with\n| 1 :: -1 :: [] -> a\n| _ -> b\n", // `::` rhs
            "match x with\n| [ -1; -2 ] -> a\n| _ -> b\n", // list elements
            "let f -1 = 0\n",                           // function-form literal arg
        ] {
            let parse = parse(src);
            assert!(
                parse.errors.is_empty(),
                "{src:?} (folded constant in a continuation position) must parse \
                 cleanly, got: {:?}",
                parse.errors
            );
        }
    }

    #[test]
    fn fold_arg_after_paren_does_not_overpromote_inner() {
        // `let f (x) -1 = …`: the inner `x`'s *filtered*-after lookahead sees
        // `-1` past the swallowed `)`, but its *raw*-after is `)` (not a fold
        // sign), so `x` must stay a `Named` value — only the outer `f` is
        // function-form. (codex round-3 P2: the folded-arg promotion must keep
        // the swallowed-`)` guard.)
        let t = format!("{:#?}", parse("let f (x) -1 = 0\n").root);
        assert!(
            t.contains("NAMED_PAT@7..8"),
            "inner `x` must stay a NAMED_PAT, not be promoted to LONG_IDENT_PAT:\n{t}"
        );
    }

    #[test]
    fn leading_zero_int8_min_is_rescued() {
        // int8/int16 use FCS's *value*-based `isInt8BadMax`/`isInt16BadMax`, so
        // leading-zero spellings of `|MinValue|` ARE rescued under `-`.
        for src in ["-0128y\n", "-032768s\n"] {
            let parse = parse(src);
            assert!(
                parse.errors.is_empty(),
                "{src:?} (value-based MinValue rescue) must parse cleanly, got: {:?}",
                parse.errors
            );
        }
    }
}
