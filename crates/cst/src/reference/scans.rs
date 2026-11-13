//! The lexer-level diagnostic scans over the raw token table, as they stood
//! before Stage 7 of `docs/token-pipeline-soa-plan.md` reordered them: FS1161
//! tabs, FS0046 reserved identifiers, and `:`/`$` in operator names. The live
//! scans (`parser::lexical_diagnostics`) must report exactly what these do.

use crate::lexer::TokenKind;
use crate::parser::ParseError;
use crate::tokens::{RawKind, RawTokens};

/// The three scans' diagnostics, in the order the live scans report them:
/// tab errors, reserved-identifier warnings, operator-character errors.
pub fn lexical_diagnostics(source: &str, raw_tokens: &RawTokens<'_>) -> [Vec<ParseError>; 3] {
    [
        tab_diagnostics(source, raw_tokens),
        reserved_ident_diagnostics(raw_tokens),
        operator_char_diagnostics(raw_tokens),
    ]
}

/// FS1161 "TABs are not allowed in F# code". FCS's lexer splits whitespace
/// into `truewhite = [' ']` and `offwhite = ['\t']`; a maximal run of tabs
/// consumed by the main token rule (`offwhite+`, `lex.fsl:705`) is a
/// recoverable error — FCS reports it against the tab run's range, then treats
/// the run as ordinary whitespace, so the tree is unchanged. One diagnostic per
/// maximal `\t` run mirrors `offwhite+`'s maximal munch: `let x =\t\t1` is one
/// error spanning both tabs, and a tab flanked by spaces (`truewhite`) flags
/// only the tab.
///
/// We flag tabs in code-position [`crate::lexer::Token::Whitespace`] only — the whole token
/// is `truewhite`/`offwhite`, so every tab in it is `offwhite+`. Tabs inside
/// comments, strings, and char literals are part of those tokens (never a
/// `Whitespace` token), and `#if`-eliminated regions arrive as
/// [`crate::directives::TriviaToken::InactiveCode`] (FCS lexes them under `skip`, no diagnostic) —
/// so both are skipped for free.
///
/// **Directive lines are deliberately not diagnosed.** Whether a tab on a
/// `#…` line is `offwhite+` (→ FS1161) or `anywhite` (swallowed) depends on the
/// exact directive rule FCS matched: `#if`/`#else`/`#elif`/`#endif` and
/// `#nowarn`/`#warnon` carry an `anywhite*` prefix (`lex.fsl:1010` &c., so even
/// a leading tab is swallowed); `#line`/bare-`# N` begin at `#` with no prefix
/// (`:757`, so a leading tab *is* flagged); `#light`/`#indent` match
/// `"#light" anywhite* newline` / `(… ) anywhite+ "on"|"off"` (`:999-1006`);
/// and there are invalid-directive recovery rules (`anywhite* "#if" ident_char+`
/// &c.). Reproducing that grammar faithfully would mean re-implementing FCS's
/// directive lexer for inputs — tabs around directives — that essentially never
/// occur in real code. This LSP doesn't model that grammar (`lexfilter/mod.rs`),
/// so we decline to diagnose tabs on any line whose first non-blank byte is `#`
/// ([`whitespace_on_directive_line`]) rather than approximate it case by case.
/// The trade is deliberate: false negatives on (pathological) directive-line
/// tabs, never a false positive. Ordinary code — the case that matters — is
/// unaffected, since a code line's first non-blank byte isn't `#`.
fn tab_diagnostics(source: &str, raw_tokens: &RawTokens<'_>) -> Vec<ParseError> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    for i in 0..raw_tokens.len() {
        let span = raw_tokens.span(i);
        // The directive-line test scans back to the start of the line, so it
        // runs only for whitespace that holds a tab; run for every whitespace
        // token, it would make a long line quadratic.
        if raw_tokens.kind(i) != RawKind::Lexed(TokenKind::Whitespace)
            || !bytes[span.clone()].contains(&b'\t')
            || whitespace_on_directive_line(bytes, span.start)
        {
            continue;
        }
        let mut i = span.start;
        while i < span.end {
            if bytes[i] != b'\t' {
                i += 1;
                continue;
            }
            let run_start = i;
            while i < span.end && bytes[i] == b'\t' {
                i += 1;
            }
            out.push(ParseError {
                message: "TABs are not allowed in F# code".to_string(),
                span: run_start..i,
            });
        }
    }
    out
}

/// FS0046 reserved-identifier warnings: a lexer-level diagnostic scanned off the
/// active-code identifier tokens. F#'s ML-compatibility reserved words (`break`,
/// `sealed`, `tailcall`, … — see [`crate::lexer::RESERVED_IDENTS`]) lex as plain
/// identifiers, but FCS's `KeywordOrIdentifierToken` emits a *warning* for each
/// occurrence before handing the parser an `IDENT`. This mirrors that: one
/// warning per reserved word used as an identifier, at its exact span.
///
/// Only real active-code tokens (`TriviaToken::Lexed`) are scanned — inactive
/// (`#if`-eliminated) regions never lex to `Token::Ident`, and backtick-quoted
/// `` ``break`` `` lexes as `Token::QuotedIdent`, so neither draws a warning,
/// matching FCS.
///
/// The warning's span mirrors FCS's `LexemeRange`, which covers the *whole*
/// matched lexeme — and FCS folds a glued `!`/`#` into the identifier lexeme
/// (both `break!` and `break#` lex as one `Identifier` token spanning the
/// suffix). So:
/// * A glued `#` **suppresses** the warning. FCS looks up the full `break#`
///   lexeme (`lex.fsl:367`); it isn't reserved, so only FS1141 fires, no FS0046.
/// * A glued `!` **keeps** the warning but **extends its span over the `!`**.
///   FCS's `ident '!'` rule (`lex.fsl:360`) looks up the *trimmed* `break` (so
///   FS0046 still fires) while retaining the full `break!` `LexemeRange`.
/// * A space breaks the composite: `break #` / `break !` warn over just `break`.
///
/// Our lexer splits both suffixes off as separate `Hash` / `Op("!")` tokens and
/// doesn't yet emit FS1141 for any identifier (the general `ident '!'`/`ident
/// '#'` gap — `foo!` and `foo#` diverge too), so the accept/reject signal for
/// `break!` is not yet faithful; only the FS0046 emission (presence and span) is
/// matched here.
/// FCS's `checkExprOp` (`lex.fsl:110`): an operator lexeme containing `:` or
/// `$` is an error ("'…' is not permitted as a character in operator names"),
/// reported once per offending character over the whole lexeme. The
/// fixed-spelling tokens that legitimately carry those characters (`::`, `:=`,
/// `:>`, `:?>`, a bare `$`) never lex as [`crate::lexer::Token::Op`], so every `Op` is subject
/// to the check — except that the `>`-headed rule (`checkExprGreaterColonOp`,
/// `lex.fsl:983`) checks only `$`. That rule applies when the first character
/// past the leading `ignored_op_char`s is `>`; a `$` among those leading
/// characters makes the lexeme an error regardless of which rule claims it.
fn operator_char_diagnostics(raw_tokens: &RawTokens<'_>) -> Vec<ParseError> {
    let mut out = Vec::new();
    for i in 0..raw_tokens.len() {
        if raw_tokens.kind(i) != RawKind::Lexed(TokenKind::Op) {
            continue;
        }
        let (op, span) = (raw_tokens.text(i), raw_tokens.span(i));
        let greater_headed = op.trim_start_matches(['.', '?']).starts_with('>');
        for c in [':', '$'] {
            if op.contains(c) && !(c == ':' && greater_headed) {
                out.push(ParseError {
                    message: format!(
                        "'{c}' is not permitted as a character in operator names and is \
                         reserved for future use"
                    ),
                    span: span.clone(),
                });
            }
        }
    }
    out
}

fn reserved_ident_diagnostics(raw_tokens: &RawTokens<'_>) -> Vec<ParseError> {
    let mut out = Vec::new();
    for i in 0..raw_tokens.len() {
        let (s, span) = (raw_tokens.text(i), raw_tokens.span(i));
        if raw_tokens.kind(i) == RawKind::Lexed(TokenKind::Ident)
            && crate::lexer::is_reserved_ident(s)
        {
            match glued_ident_suffix(raw_tokens, i, span.end) {
                // `break#` — folded into a non-reserved lexeme, no FS0046.
                Some(GluedSuffix::Hash) => continue,
                // `break!` — FS0046 over the full `break!` lexeme.
                Some(GluedSuffix::Bang(bang_end)) => out.push(ParseError {
                    message: format!("The identifier '{s}' is reserved for future use by F#"),
                    span: span.start..bang_end,
                }),
                // Bare `break` — FS0046 over the word itself.
                None => out.push(ParseError {
                    message: format!("The identifier '{s}' is reserved for future use by F#"),
                    span: span.clone(),
                }),
            }
        }
    }
    out
}

/// A `!` or `#` FCS would fold into the identifier lexeme: the token at `i + 1`
/// begins exactly at `ident_end` (no intervening trivia) and is a `#` (`Hash`)
/// or an operator whose first char is `!`. See [`reserved_ident_diagnostics`]
/// for how each shapes the FS0046 warning; the `Bang` variant carries the end of
/// the single consumed `!` so the warning span reaches the full `break!` lexeme.
///
/// FCS's `ident '!'` rule (`lex.fsl:360`) consumes exactly *one* trailing `!`,
/// but Logos greedily lexes a run of operator chars into one token — `break!!`
/// and `break!=` yield `Op("!!")` / `Op("!=")`. So any glued operator starting
/// with `!` extends the span by exactly one byte (`!` is ASCII), matching FCS
/// (verified via fcs-dump: `break!!`, `break!=`, `break!+` all warn over cols
/// 8..14, i.e. `break!`).
enum GluedSuffix {
    Hash,
    Bang(usize),
}

fn glued_ident_suffix(
    raw_tokens: &RawTokens<'_>,
    i: usize,
    ident_end: usize,
) -> Option<GluedSuffix> {
    let next = i + 1;
    if next >= raw_tokens.len() || raw_tokens.span(next).start != ident_end {
        return None;
    }
    match raw_tokens.kind(next) {
        RawKind::Lexed(TokenKind::Hash) => Some(GluedSuffix::Hash),
        RawKind::Lexed(TokenKind::Op) if raw_tokens.text(next).starts_with('!') => {
            Some(GluedSuffix::Bang(ident_end + 1))
        }
        _ => None,
    }
}

/// Is the whitespace token at `ws_start` on a `#…` directive line — i.e. is the
/// first non-blank byte of its line a `#`? Such lines (`#if`, `#light`, `#line`,
/// `#nowarn`, the invalid forms, …) are not diagnosed for tabs; see
/// [`tab_diagnostics`] for why. Covers both a directive's leading indent and any
/// whitespace within it, since both share the line's `#` first-non-blank byte.
fn whitespace_on_directive_line(bytes: &[u8], ws_start: usize) -> bool {
    let line_start = bytes[..ws_start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let first_non_blank = bytes[line_start..]
        .iter()
        .position(|&b| !matches!(b, b' ' | b'\t'))
        .map(|off| line_start + off);
    first_non_blank.is_some_and(|i| bytes[i] == b'#')
}
