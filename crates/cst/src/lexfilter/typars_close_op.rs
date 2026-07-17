//! Typar-application disambiguation helpers — the `TyparsCloseOp` active
//! pattern and the token classifier driving `peek_adjacent_typars`. Kept in
//! a sibling module to its parent `lexfilter` because the unit is large
//! (struct + two free functions + a five-variant enum + an exhaustive
//! property-test suite) and orthogonal to the rest of `LexFilter`: the
//! parent's `impl Filter` methods consume it through three small entry
//! points but the helpers themselves don't touch `Filter` state.

use super::TokenContent;
use crate::lexer::Token;

/// Result of [`typars_close_op_split`]: a leading run of `>` characters
/// (each becoming a `Greater(false)` token) followed by an optional tail
/// classified as a single token.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TyparsCloseSplit<'a> {
    /// Number of leading `>` chars (and thus `Greater(false)` tokens to emit).
    pub(super) greater_count: usize,
    /// Tail token, if any. `None` when the op text is all `>`s.
    pub(super) tail: Option<Token<'a>>,
}

/// FCS's `TyparsCloseOp` active pattern (LexFilter.fs:530-579). Splits a
/// `>`-prefixed operator string into a run of `>` characters plus an optional
/// tail token. Used during the typar-application scan to recognise the
/// closing `>` of a generic application even when the lexer fused it with
/// adjacent operator chars (e.g. `>>` for nested generics, `>=` would-be,
/// `>.` for property access on a generic call).
///
/// Returns `None` when the input does not start with `>` *or* when the tail
/// after the `>`-run is one FCS itself would reject (e.g. a leading `:` —
/// not in any `StartsWith` arm of the FCS pattern). The tail is `None`
/// when the entire input is `>`s.
///
/// FCS classifies the tail into specific INFIX_* token kinds; our lexer
/// represents most operator strings as `Token::Op(&str)`, so the tail
/// collapses to either a dedicated single-purpose token (`Dot`, `Equals`,
/// `RArrow`, …) or a generic `Op(slice)` for multi-char operators. The
/// returned `Token`s borrow from the input string.
pub(super) fn typars_close_op_split(op_text: &str) -> Option<TyparsCloseSplit<'_>> {
    if !op_text.starts_with('>') {
        return None;
    }
    let greater_count = op_text.bytes().take_while(|&b| b == b'>').count();
    let after = &op_text[greater_count..];
    let tail = match after {
        "" => None,
        // Exact matches: FCS emits dedicated tokens here. Mirror them with
        // our equivalent variants where they exist; for the cases FCS
        // gives a dedicated token (MINUS, STAR, PERCENT_OP) that we
        // represent as a generic `Op(_)`, fall through to that.
        "." => Some(Token::Dot),
        "]" => Some(Token::RBrack),
        "-" | "*" | "%" | "%%" => Some(Token::Op(after)),
        ".." => Some(Token::DotDot),
        "?" => Some(Token::QMark),
        "??" => Some(Token::QMarkQMark),
        ":=" => Some(Token::ColonEquals),
        "::" => Some(Token::ColonColon),
        "&" => Some(Token::Amp),
        "->" => Some(Token::RArrow),
        "<-" => Some(Token::LArrow),
        "=" => Some(Token::Equals),
        "<" => Some(Token::Less(false)),
        "$" => Some(Token::Dollar),
        // Catch-all: FCS uses `StartsWith` arms keyed on the first char.
        // Anything not in this set (notably a leading `:`, `,`, `;`,
        // `(`, `)`, etc.) is rejected outright. Our `Op` regex only
        // contains the operator-character class, so the only realistic
        // rejection in practice is `:`-leading tails like `>::` (already
        // a literal match) and `>:` (rejected here).
        _ => {
            let first = after.as_bytes()[0];
            let accepted = matches!(
                first,
                b'=' | b'<'
                    | b'>'
                    | b'$'
                    | b'&'
                    | b'|'
                    | b'!'
                    | b'?'
                    | b'~'
                    | b'@'
                    | b'^'
                    | b'+'
                    | b'-'
                    | b'*'
                    | b'/'
                    | b'%'
            );
            if !accepted {
                return None;
            }
            Some(Token::Op(after))
        }
    };
    Some(TyparsCloseSplit {
        greater_count,
        tail,
    })
}

/// Tokens that, when followed by an adjacent `<`, trigger the typar
/// application disambiguation pass. Mirrors FCS LexFilter.fs:2659
/// (`DELEGATE | IDENT _ | IEEE64 _ | … | BIGNUM _`).
pub(super) fn is_typar_application_trigger(t: &TokenContent<'_>) -> bool {
    match t {
        TokenContent::Real(real) => matches!(
            real,
            Token::Ident(_)
                | Token::QuotedIdent(_)
                | Token::Delegate
                | Token::Int(_)
                | Token::IntSuffixed(_)
                | Token::XInt(_)
                | Token::XIntSuffixed(_)
                | Token::XIEEE32(_)
                | Token::XIEEE64(_)
                | Token::Float32(_)
                | Token::Float64(_)
                | Token::Decimal(_)
                | Token::BigNum(_)
        ),
        _ => false,
    }
}

/// What does a token mean when seen inside the `peek_adjacent_typars`
/// scan loop? Mirrors the dispatch arms of FCS `LexFilter.fs:1096-1188`.
pub(super) enum TyparScanAction {
    /// EOF or `;;` — terminate the scan with failure (unless we just
    /// happened to be at `n_paren == 0` from a successful close arm,
    /// which is handled before reaching this dispatch).
    Fail,
    /// A whitelist token that doesn't affect paren balance — IDENT,
    /// literal, comma, `*`, `/`, `^`, `->`, `:`, `_`, struct, etc.
    Continue,
    /// `(`, `[`, `[<`, `<@`, `<`, `</`, `<^` — increment paren depth.
    /// (The two infix-compare opener strings are also nested opener
    /// candidates per FCS line 1138-1141.)
    OpenParen,
    /// `)`, `]` — decrement; on reaching `n_paren == 0` the scan fails
    /// (these are not valid typar closers, only `>` is).
    ClosePlain,
    /// Bare `Greater(_)` — decrement; on `n_paren == 0` the scan succeeds.
    CloseGreater,
    /// `>]` (`GreaterRBrack`) — decrement; on `n_paren == 0` the scan
    /// succeeds. Implicit `has_tail = true` (the `]` is the after-op).
    CloseGreaterWithAfter,
    /// `Op(s)` where `s` starts with `>` and `typars_close_op_split` accepts
    /// it. Decrement `n_paren` by `greater_count`; on reaching zero the
    /// scan succeeds, otherwise continue.
    CloseOpSplit {
        greater_count: usize,
        has_tail: bool,
    },
    /// Anything else: per FCS, allowed only when `n_paren > 1` (i.e.
    /// strictly nested) — otherwise fail.
    OtherToken,
}

/// Per-token dispatch inside the typar scan loop. Pure function over
/// `TokenContent`; doesn't touch lexer state.
pub(super) fn classify_typar_scan_token(t: &TokenContent<'_>) -> TyparScanAction {
    match t {
        TokenContent::Eof => TyparScanAction::Fail,
        TokenContent::Real(real) => match real {
            Token::SemiSemi => TyparScanAction::Fail,
            // Openers.
            Token::LParen | Token::LBrack | Token::LBrackLess | Token::Less(_) | Token::LQuote => {
                TyparScanAction::OpenParen
            }
            Token::Op(s) if *s == "</" || *s == "<^" => TyparScanAction::OpenParen,
            // Plain closers that fail at depth 0.
            Token::RParen | Token::RBrack => TyparScanAction::ClosePlain,
            // Greater closers.
            Token::Greater(_) => TyparScanAction::CloseGreater,
            Token::GreaterRBrack => TyparScanAction::CloseGreaterWithAfter,
            // `>`-prefixed fused operator: split + classify.
            Token::Op(s) if s.starts_with('>') => match typars_close_op_split(s) {
                Some(split) => TyparScanAction::CloseOpSplit {
                    greater_count: split.greater_count,
                    has_tail: split.tail.is_some(),
                },
                None => TyparScanAction::OtherToken,
            },
            // Whitelisted in-grammar tokens (FCS LexFilter.fs:1158-1180).
            Token::Default
            | Token::Colon
            | Token::ColonGreater
            | Token::Struct
            | Token::Null
            | Token::Delegate
            | Token::And
            | Token::When
            | Token::Amp
            | Token::Bar
            | Token::DotDot
            | Token::New
            | Token::LBraceBar
            | Token::Semi
            | Token::BarRBrace
            | Token::Global
            | Token::Const
            | Token::Dot
            | Token::Underscore
            | Token::Equals
            | Token::Comma
            | Token::RArrow
            | Token::Hash
            | Token::Quote
            | Token::True
            | Token::False
            | Token::Ident(_)
            | Token::QuotedIdent(_)
            | Token::KeywordString(_)
            | Token::Int(_)
            | Token::IntSuffixed(_)
            | Token::XInt(_)
            | Token::XIntSuffixed(_)
            | Token::XIEEE32(_)
            | Token::XIEEE64(_)
            | Token::Float32(_)
            | Token::Float64(_)
            | Token::Decimal(_)
            | Token::BigNum(_)
            | Token::String
            | Token::TripleString
            | Token::VerbatimString
            | Token::Char(_) => TyparScanAction::Continue,
            // FCS-permitted infix-operator strings inside the typar grammar
            // at depth 1 (LexFilter.fs:1165-1179): `^`, `^-`, `/`, `*`, `-`.
            // Notably `+` and `**` are NOT in the FCS whitelist — they make
            // the scan backtrack (caller sees comparison, not typar).
            Token::Op(s) if matches!(*s, "^" | "^-" | "/" | "*" | "-") => TyparScanAction::Continue,
            _ => TyparScanAction::OtherToken,
        },
        TokenContent::Err(_) | TokenContent::Virtual(_) | TokenContent::Dummy { .. } => {
            TyparScanAction::OtherToken
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(s: &str) -> Option<TyparsCloseSplit<'_>> {
        typars_close_op_split(s)
    }

    /// Byte length a `typars_close_op_split` output token would occupy in
    /// the source. Test-only: lets the round-trip property assert that
    /// `greater_count + len(tail) == len(input)` for accepted inputs.
    fn token_byte_len(t: &Token<'_>) -> usize {
        match t {
            Token::Less(_) | Token::Greater(_) => 1,
            Token::Dot
            | Token::QMark
            | Token::Equals
            | Token::Amp
            | Token::Dollar
            | Token::Bar
            | Token::RBrack => 1,
            Token::DotDot
            | Token::QMarkQMark
            | Token::ColonEquals
            | Token::ColonColon
            | Token::RArrow
            | Token::LArrow => 2,
            Token::Op(s) => s.len(),
            other => panic!("token_byte_len: unexpected token {other:?}"),
        }
    }

    #[test]
    fn split_rejects_no_leading_gt() {
        assert!(split("=").is_none());
        assert!(split("<").is_none());
        assert!(split("").is_none());
        assert!(split(" >").is_none());
    }

    #[test]
    fn split_rejects_unaccepted_tail_first_char() {
        // FCS's TyparsCloseOp returns ValueNone for tails whose first char
        // isn't in the accepted set. Notable case: `:`-leading tails.
        assert!(split(">:").is_none());
        // `>::` is a literal-match arm and is accepted (tail = ColonColon).
        assert!(split(">::").is_some());
    }

    #[test]
    fn split_pure_gts() {
        let r = split(">").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, None);

        let r = split(">>").unwrap();
        assert_eq!(r.greater_count, 2);
        assert_eq!(r.tail, None);

        let r = split(">>>").unwrap();
        assert_eq!(r.greater_count, 3);
        assert_eq!(r.tail, None);
    }

    #[test]
    fn split_known_tails() {
        let r = split(">=").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::Equals));

        let r = split(">.").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::Dot));

        let r = split(">..").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::DotDot));

        let r = split(">]").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::RBrack));

        let r = split(">->").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::RArrow));

        let r = split(">::").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::ColonColon));

        let r = split(">&").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::Amp));

        let r = split(">$").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::Dollar));

        let r = split("><").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::Less(false)));
    }

    #[test]
    fn split_fallback_to_op() {
        let r = split(">|").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::Op("|")));

        let r = split(">@@").unwrap();
        assert_eq!(r.greater_count, 1);
        assert_eq!(r.tail, Some(Token::Op("@@")));
    }

    #[test]
    fn split_mixed_gt_and_tail() {
        // `>>=` — two `>`s plus `=`.
        let r = split(">>=").unwrap();
        assert_eq!(r.greater_count, 2);
        assert_eq!(r.tail, Some(Token::Equals));

        // `>>.` — two `>`s plus `.`.
        let r = split(">>.").unwrap();
        assert_eq!(r.greater_count, 2);
        assert_eq!(r.tail, Some(Token::Dot));
    }

    #[test]
    fn token_byte_len_singles() {
        assert_eq!(token_byte_len(&Token::Greater(false)), 1);
        assert_eq!(token_byte_len(&Token::Less(true)), 1);
        assert_eq!(token_byte_len(&Token::Dot), 1);
        assert_eq!(token_byte_len(&Token::Equals), 1);
        assert_eq!(token_byte_len(&Token::Amp), 1);
        assert_eq!(token_byte_len(&Token::Dollar), 1);
        assert_eq!(token_byte_len(&Token::RBrack), 1);
        assert_eq!(token_byte_len(&Token::QMark), 1);
        assert_eq!(token_byte_len(&Token::Bar), 1);
    }

    #[test]
    fn token_byte_len_doubles() {
        assert_eq!(token_byte_len(&Token::DotDot), 2);
        assert_eq!(token_byte_len(&Token::QMarkQMark), 2);
        assert_eq!(token_byte_len(&Token::ColonEquals), 2);
        assert_eq!(token_byte_len(&Token::ColonColon), 2);
        assert_eq!(token_byte_len(&Token::RArrow), 2);
        assert_eq!(token_byte_len(&Token::LArrow), 2);
    }

    #[test]
    fn token_byte_len_op() {
        assert_eq!(token_byte_len(&Token::Op("**")), 2);
        assert_eq!(token_byte_len(&Token::Op("|>>")), 3);
        assert_eq!(token_byte_len(&Token::Op("@@")), 2);
    }

    #[test]
    fn split_roundtrips_textually() {
        // For every input the helper accepts, summing the byte lengths of
        // the emitted tokens reproduces the original input length. This is
        // the invariant peek_adjacent_typars will rely on to compute sub-
        // spans after a successful close-op split.
        for s in [
            ">", ">>", ">>>", ">=", ">.", ">..", ">]", ">->", ">::", ">&", ">$", "><", ">|", ">@@",
            ">>=", ">>.",
        ] {
            let r = split(s).unwrap();
            let tail_len = r.tail.as_ref().map(token_byte_len).unwrap_or(0);
            assert_eq!(
                r.greater_count + tail_len,
                s.len(),
                "byte-length round-trip mismatch for {s:?}: split={r:?}"
            );
        }
    }

    /// Enumerate every non-empty ASCII string of length up to `max_len`
    /// over `alphabet`. Used to exhaustively check the helper's
    /// properties on a small but representative domain.
    fn enumerate_strings(alphabet: &[u8], max_len: usize) -> Vec<String> {
        let mut out: Vec<String> = vec![String::new()];
        for len in 1..=max_len {
            let n = alphabet.len();
            let mut total = 1usize;
            for _ in 0..len {
                total *= n;
            }
            for i in 0..total {
                let mut s = String::with_capacity(len);
                let mut j = i;
                for _ in 0..len {
                    s.push(alphabet[j % n] as char);
                    j /= n;
                }
                out.push(s);
            }
        }
        out
    }

    /// Alphabet covering the structurally distinct cases:
    /// - `>` is the prefix character and also a valid tail char.
    /// - `=`, `&`, `?`, `<`, `-` are tail chars in the accepted byte set
    ///   and several appear in the literal-match arms.
    /// - `.`, `]` are literal-match-only chars (not in the byte set).
    /// - `:` is *not* in any accepted set — its presence as the
    ///   tail-first-char must cause `None`.
    const PROP_ALPHABET: &[u8] = b">=.:&?<-]";

    #[test]
    fn prop_accepted_iff_leading_gt_and_valid_tail_first_char() {
        // Acceptance criterion: split(s) is Some iff
        //   s starts with '>'  AND
        //   ( the post-'>'-run is empty OR
        //     it's one of the literal-match arms OR
        //     its first byte is in the accepted byte set ).
        // The literal-match arms in the helper above are enumerated
        // explicitly so we don't double-spec them.
        const LITERAL_TAILS: &[&str] = &[
            ".", "]", "-", "*", "%", "%%", "..", "?", "??", ":=", "::", "&", "->", "<-", "=", "<",
            "$",
        ];
        const ACCEPTED_FIRST_BYTES: &[u8] = b"=<>$&|!?~@^+-*/%";

        for s in enumerate_strings(PROP_ALPHABET, 3) {
            let actual = split(&s);
            let expected_some = s.starts_with('>') && {
                let after = &s[s.bytes().take_while(|&b| b == b'>').count()..];
                after.is_empty()
                    || LITERAL_TAILS.contains(&after)
                    || ACCEPTED_FIRST_BYTES.contains(&after.as_bytes()[0])
            };
            assert_eq!(
                actual.is_some(),
                expected_some,
                "acceptance mismatch for {s:?}: actual={actual:?}"
            );
        }
    }

    #[test]
    fn prop_greater_count_matches_leading_gt_run() {
        for s in enumerate_strings(PROP_ALPHABET, 3) {
            if let Some(r) = split(&s) {
                let expected = s.bytes().take_while(|&b| b == b'>').count();
                assert_eq!(
                    r.greater_count, expected,
                    "greater_count mismatch for {s:?}: split={r:?}"
                );
            }
        }
    }

    #[test]
    fn prop_tail_none_iff_input_is_all_gts() {
        for s in enumerate_strings(PROP_ALPHABET, 3) {
            if let Some(r) = split(&s) {
                let all_gts = !s.is_empty() && s.bytes().all(|b| b == b'>');
                assert_eq!(
                    r.tail.is_none(),
                    all_gts,
                    "tail-None mismatch for {s:?}: split={r:?}"
                );
            }
        }
    }

    #[test]
    fn int_dot_dot_is_not_a_typar_trigger() {
        // FCS LexFilter.fs:2659 omits INT32_DOT_DOT from the typar
        // trigger list. Codex review caught a divergence where our
        // implementation accepted IntDotDot — fixed by removal.
        let tok = TokenContent::Real(Token::IntDotDot("1.."));
        assert!(!is_typar_application_trigger(&tok));
    }

    #[test]
    fn int_dot_dot_is_other_token_in_typar_scan() {
        // FCS LexFilter.fs:1173-1175 omits INT32_DOT_DOT from the
        // depth-1 whitelist. An IntDotDot reached inside a scan must
        // dispatch to OtherToken (rejected at n_paren <= 1).
        let tok = TokenContent::Real(Token::IntDotDot("1.."));
        assert!(matches!(
            classify_typar_scan_token(&tok),
            TyparScanAction::OtherToken
        ));
    }

    #[test]
    fn prop_byte_length_roundtrip_exhaustive() {
        for s in enumerate_strings(PROP_ALPHABET, 3) {
            if let Some(r) = split(&s) {
                let tail_len = r.tail.as_ref().map(token_byte_len).unwrap_or(0);
                assert_eq!(
                    r.greater_count + tail_len,
                    s.len(),
                    "byte-length round-trip mismatch for {s:?}: split={r:?}"
                );
            }
        }
    }
}
