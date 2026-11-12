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
