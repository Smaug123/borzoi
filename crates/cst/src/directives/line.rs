//! Line-oriented recogniser for hash directives.
//!
//! Two flavours of single-line `#`-directive:
//!
//! 1. The four **conditional-compilation** directives (`#if` / `#elif` /
//!    `#else` / `#endif`), which drive the preprocessor state machine.
//!    Mirrors the patterns in FCS's `src/Compiler/lex.fsl`:
//!
//!    ```text
//!    anywhite* "#if"    anywhite+ anystring
//!    anywhite* "#elif"  anywhite+ anystring
//!    anywhite* "#else"  anywhite* ("//" anystring)?
//!    anywhite* "#endif" anywhite* ("//" anystring)?
//!    ```
//!
//! 2. The three **trivia** directives (`#nowarn` / `#warnon` / `#line`),
//!    plus the alternate bare-numeric form of `#line` that fslex / fsyacc
//!    emit (e.g. `# 1 "fsyacclex.fsl"`). FCS recognises these at the
//!    lexer level and swallows them under `SkipTrivia`; we mirror that by
//!    returning a payload-less variant the driver discards without
//!    touching the ifdef stack or emitting a token. From `lex.fsl`:
//!
//!    ```text
//!    anywhite* ("#nowarn" | "#warnon") anystring
//!    ('#' anywhite* | "#line" anywhite+) digit+ anywhite*
//!       ('@'? "\"" [^'\n''\r''"']+ '"')? anywhite* newline
//!    ```
//!
//! `anywhite` is the FCS character class `[' ' '\t']`. Directives are
//! single-line: they consume from `line_start` up to (but not including) the
//! line-terminating `\n`. The `#` may be preceded only by horizontal
//! whitespace, mirroring F#'s rule that the directive must be the first
//! non-whitespace token on its line (FCS calls this `shouldStartLine`; in
//! the wrapper-style architecture used here, the driver only invokes this
//! recogniser at fresh line starts, and `(**)#if` is rejected because the
//! `(` shows up before the `#`).
//!
//! `#if` / `#elif` require at least one whitespace character separating the
//! keyword from the expression body — matching FCS, where the grammar
//! literally writes `anywhite+`. Without it, the line is reported as
//! `MissingSeparator` (this includes `#if(FOO)`, which FCS also rejects).
//! When the separator is present but the body is empty or comment-only,
//! the error is `MissingExpression` instead.
//!
//! `#else` / `#endif` take no expression. They allow trailing horizontal
//! whitespace and an optional `// ...` line comment; anything else after
//! the keyword is `UnexpectedTokensAfterDirective`.
//!
//! `#nowarn` / `#warnon` swallow the whole line regardless of body shape
//! (FCS rule is `anywhite* ("#nowarn"|"#warnon") anystring`). There is *no*
//! word-boundary requirement after the keyword: FCS matches `#nowarn40`,
//! `#warnonx`, `#nowarnings`, … and leaves later passes to diagnose them.
//!
//! `#line` requires the FCS-strict shape
//!   `"#line" anywhite+ digit+ anywhite* ('@'? "\"" [^"\n\r]+ '"')? anywhite* newline`.
//! Bare `#line`, `#line foo`, `#line 5 garbage`, `#line 5 "unterminated`,
//! and `#line 5 @foo` (no quotes after the `@`) all fail the FCS regex
//! and fall through to ordinary lexing. The bare-numeric `# N "file"`
//! alternate uses the same tail validation. FCS's `newline` class is
//! `'\n' | '\r' '\n'` — so a `#line` line running into EOF, or one ending
//! with a lone `\r`, does not match the rule and is left to ordinary lexing.
//!
//! `#ifdef`, `#elseif`, `#load`, `#r`, `#light`, …: not handled — the
//! recogniser returns `None`, and the caller continues normal lexing.

use super::expr::{ParseError as ExprParseError, parse_if_expr};
use crate::directives::expr::Expr;
use std::ops::Range;

/// A warning number parsed from a `#nowarn` / `#warnon` directive argument.
///
/// Mirrors FCS, which parses each argument with `Int32.TryParse` after
/// stripping quotes and an optional `FS` prefix (see
/// `src/Compiler/SyntaxTree/WarnScopes.fs`, `getNumber`). The stored value can
/// therefore be negative or larger than any real F# warning number; deciding
/// which values are meaningful is left to the consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WarningNumber(pub i32);

/// A recognised hash directive — either a conditional-compilation directive
/// that drives the preprocessor state machine, or a trivia directive
/// (`#nowarn` / `#warnon` / `#line`) that the driver silently consumes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Directive {
    If(Expr),
    Elif(Expr),
    Else,
    EndIf,
    /// `#nowarn ...` — suppress the listed warning(s) for the rest of the
    /// file. `numbers` holds the parsed warning numbers in source order.
    /// Arguments FCS can't parse as a warning number — and a keyword glued to
    /// a non-whitespace byte (e.g. `#nowarn40`, which FCS reads as the
    /// directive identifier `nowarn40`) — yield an empty list.
    NoWarn {
        numbers: Vec<WarningNumber>,
    },
    /// `#warnon ...` — re-enable the listed warning(s) for the rest of the
    /// file. Symmetric to [`Directive::NoWarn`].
    WarnOn {
        numbers: Vec<WarningNumber>,
    },
    /// `#line N "file"` or the bare-numeric alternate `# N "file"` —
    /// source-position pragma emitted by fslex / fsyacc and other code
    /// generators. `number` is the virtual line number (0 if the digit run
    /// overflows `i32`, mirroring FCS's overflow-to-0); `file` is the raw
    /// filename text between the quotes (no unescaping; a leading `@` is
    /// ignored), or `None` when the directive carries only a line number.
    Line {
        number: u32,
        file: Option<String>,
    },
}

/// The four CC directive keywords as a payload-free tag. Returned alongside
/// [`DirectiveError`] so the driver can update its ifdef stack consistently
/// when the directive body fails to parse — `#elif` with a bad expression
/// still pops/pushes the stack differently from a bad `#if`, and `#endif x`
/// must still close a frame.
///
/// Trivia directives (`#nowarn` / `#warnon` / `#line`) have no
/// `DirectiveKind` variant: they are state-neutral, and the driver
/// short-circuits them by checking [`Directive::is_trivia`] before
/// calling [`Directive::kind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectiveKind {
    If,
    Elif,
    Else,
    EndIf,
}

impl Directive {
    /// The CC directive kind, if any. `None` for trivia directives
    /// (`#nowarn` / `#warnon` / `#line`), which don't affect the ifdef
    /// stack and so don't fit into the `DirectiveKind` taxonomy.
    pub fn kind(&self) -> Option<DirectiveKind> {
        match self {
            Self::If(_) => Some(DirectiveKind::If),
            Self::Elif(_) => Some(DirectiveKind::Elif),
            Self::Else => Some(DirectiveKind::Else),
            Self::EndIf => Some(DirectiveKind::EndIf),
            Self::NoWarn { .. } | Self::WarnOn { .. } | Self::Line { .. } => None,
        }
    }

    /// True for `#nowarn` / `#warnon` / `#line` — directives that FCS
    /// recognises at the lexer level but emits no token for under
    /// `SkipTrivia`. The driver swallows them without touching the
    /// ifdef stack.
    pub fn is_trivia(&self) -> bool {
        matches!(
            self,
            Self::NoWarn { .. } | Self::WarnOn { .. } | Self::Line { .. }
        )
    }
}

/// A successfully recognised directive plus the byte range it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recognised {
    pub directive: Directive,
    /// `line_start..line_end` — covers from the start of the line through to
    /// (but excluding) the line terminator (any of `\n`, `\r`, `\r\n`), or
    /// to `source.len()` if the directive runs to end of source.
    pub range: Range<usize>,
}

/// A malformed directive. The line looked enough like `#if` / `#elif` /
/// `#else` / `#endif` to commit to the directive parse, but the body was
/// invalid. The driver still updates its ifdef state (typically treating
/// `#if`/`#elif` as `false`) and emits the error as a diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectiveError {
    /// Which CC keyword was being parsed. Lets the driver pick the right
    /// recovery action (e.g. pop the stack for a bad `#endif`).
    pub keyword: DirectiveKind,
    pub kind: DirectiveErrorKind,
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectiveErrorKind {
    /// `#if` / `#elif` lacked the whitespace separator FCS requires
    /// between the keyword and its body — either the line is just the
    /// bare keyword (`#if`, `#if<EOL>`) or the next char is not
    /// whitespace (`#if(FOO)`). FCS handles these via a separate lex
    /// rule that does *not* open a conditional section, so the driver
    /// must not push a frame either.
    MissingSeparator,
    /// `#if` / `#elif` had the required separator whitespace but no
    /// expression body — empty body or a `// ...` comment only
    /// (`#if   `, `#if // c`). FCS recognises this as a malformed
    /// well-formed directive and pushes a false frame.
    MissingExpression,
    /// The expression body parsed unsuccessfully.
    ExpressionParse(ExprParseError),
    /// `#else` / `#endif` had trailing tokens other than a `// ...` comment.
    UnexpectedTokensAfterDirective,
}

/// Try to recognise a CC directive starting at `line_start` in `source`.
///
/// `line_start` must be 0 or follow a `\n`. The recogniser is defensive: it
/// returns `None` rather than panicking if this invariant is violated.
///
/// Returns:
/// - `None` — the line isn't a CC directive (regular code, an unrelated
///   `#`-directive like `#nowarn`, an `#ifdef` / `#elseif` word-boundary
///   mismatch, an empty line, or an offset that isn't at a line start).
///   The caller continues normal lexing from `line_start`.
/// - `Some(Ok(_))` — well-formed directive.
/// - `Some(Err(_))` — malformed but unmistakably a CC directive. The driver
///   should still treat the line as consumed.
pub fn recognise_directive(
    source: &str,
    line_start: usize,
) -> Option<Result<Recognised, DirectiveError>> {
    if line_start > source.len() {
        return None;
    }
    // Line-anchored: either we're at the start of the source, or the byte
    // immediately before us is one of the three line terminators the Logos
    // lexer accepts (`\n`, `\r`, or the `\n` of a CRLF pair — in all three
    // cases the byte at `line_start - 1` is `\n` or `\r`).
    if line_start > 0 {
        let prev = source.as_bytes().get(line_start - 1).copied();
        if prev != Some(b'\n') && prev != Some(b'\r') {
            return None;
        }
    }

    let bytes = source.as_bytes();
    // The line ends at the first `\n` or `\r` — whichever comes first. For
    // a CRLF pair we stop at the `\r`, which leaves the trailing whitespace
    // / body slice clean without a separate strip step. This matches the
    // Logos lexer's `\r\n | \n | \r` newline class.
    let line_end = bytes[line_start..]
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .map(|i| line_start + i)
        .unwrap_or(source.len());

    let mut pos = line_start;

    // At the very start of source, skip a UTF-8 BOM (`U+FEFF` = `EF BB BF`).
    // The Logos lexer's whitespace class includes the BOM as well, so the
    // directive layer must too; otherwise a BOM-prefixed file with a
    // first-line directive would slip past the `#` check.
    if line_start == 0 {
        const BOM: &[u8] = b"\xEF\xBB\xBF";
        if bytes.starts_with(BOM) && BOM.len() <= line_end {
            pos += BOM.len();
        }
    }

    // Skip leading horizontal whitespace.
    while pos < line_end && matches!(bytes[pos], b' ' | b'\t') {
        pos += 1;
    }

    // Require `#`.
    if pos >= line_end || bytes[pos] != b'#' {
        return None;
    }
    pos += 1;

    // FCS lex.fsl line 1084: `anywhite* ("#nowarn" | "#warnon") anystring`.
    // No word-boundary requirement after the keyword: FCS swallows
    // `#nowarn40`, `#warnonx`, `#nowarnings`, … as warning-directive lines
    // and lets later passes diagnose them. We match the literal `nowarn` /
    // `warnon` prefix here, *before* the alphabetic-keyword loop, which
    // would otherwise over-eagerly consume `#warnonx` as keyword "warnonx"
    // and then fail to dispatch.
    if let Some(body) = source
        .get(pos..line_end)
        .and_then(|s| s.strip_prefix("nowarn"))
    {
        return Some(Ok(Recognised {
            directive: Directive::NoWarn {
                numbers: parse_warn_numbers(body),
            },
            range: line_start..line_end,
        }));
    }
    if let Some(body) = source
        .get(pos..line_end)
        .and_then(|s| s.strip_prefix("warnon"))
    {
        return Some(Ok(Recognised {
            directive: Directive::WarnOn {
                numbers: parse_warn_numbers(body),
            },
            range: line_start..line_end,
        }));
    }

    // Read the keyword. All directive keywords we recognise are ASCII lowercase,
    // so we only accept ASCII letters; an explicit word-boundary check after the
    // loop rejects continuations like `#ifdef` and `#elseif`.
    let keyword_start = pos;
    while pos < line_end && bytes[pos].is_ascii_alphabetic() {
        pos += 1;
    }
    let keyword = &source[keyword_start..pos];

    // Bare-numeric `#line` alternate: FCS `'#' anywhite* digit+ ...` — no
    // keyword letters between `#` and the digit run. fslex/fsyacc emit this
    // form (`# 1 "fsyacclex.fsl"`). Match it before the word-boundary check,
    // which would otherwise reject the digit-as-identifier-continue.
    if keyword.is_empty() {
        while pos < line_end && matches!(bytes[pos], b' ' | b'\t') {
            pos += 1;
        }
        let digit_start = pos;
        while pos < line_end && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos == digit_start {
            return None;
        }
        let number = parse_line_number(&source[digit_start..pos]);
        let file = line_directive_tail(bytes, source, pos, line_end)?;
        return Some(Ok(Recognised {
            directive: Directive::Line { number, file },
            range: line_start..line_end,
        }));
    }

    // Word boundary: the next byte must not look like an identifier continuation,
    // otherwise `#if` was actually the prefix of a longer identifier (`#ifdef`,
    // `#if5`, …). Identifier-continue per F#: digit, underscore, apostrophe,
    // or any non-ASCII byte (start of a multibyte Unicode letter / digit).
    if pos < line_end {
        let b = bytes[pos];
        if b.is_ascii_digit() || b == b'_' || b == b'\'' || b >= 0x80 {
            return None;
        }
    }

    let range = line_start..line_end;

    match keyword {
        "if" | "elif" => {
            let directive_kind = if keyword == "if" {
                DirectiveKind::If
            } else {
                DirectiveKind::Elif
            };

            // FCS requires `anywhite+` separating the keyword from the body.
            // `#if(FOO)` therefore does not match `#if anywhite+ anystring`
            // and is treated as a missing-expression error.
            let after_keyword = pos;
            while pos < line_end && matches!(bytes[pos], b' ' | b'\t') {
                pos += 1;
            }
            if pos == after_keyword {
                // Range covers only the keyword (`#if` / `#elif`), not
                // the rest of the line. The driver advances past the
                // keyword and resumes ordinary lexing — so `#if(FOO)`
                // still emits its tokens `(`, `Ident("FOO")`, `)`.
                return Some(Err(DirectiveError {
                    keyword: directive_kind,
                    kind: DirectiveErrorKind::MissingSeparator,
                    range: line_start..after_keyword,
                }));
            }

            let body = &source[pos..line_end];
            match parse_if_expr(body) {
                Ok(expr) => Some(Ok(Recognised {
                    directive: match directive_kind {
                        DirectiveKind::If => Directive::If(expr),
                        DirectiveKind::Elif => Directive::Elif(expr),
                        // Unreachable: directive_kind is If/Elif in this arm.
                        _ => unreachable!(),
                    },
                    range,
                })),
                Err(e) => {
                    // Empty body (or just a `// ...` comment) is the FCS-style
                    // "must have ident" case; everything else is a real parse
                    // error in the body.
                    let kind = if body.is_empty() || body.starts_with("//") {
                        DirectiveErrorKind::MissingExpression
                    } else {
                        DirectiveErrorKind::ExpressionParse(e)
                    };
                    Some(Err(DirectiveError {
                        keyword: directive_kind,
                        kind,
                        range,
                    }))
                }
            }
        }
        "else" | "endif" => {
            let directive_kind = if keyword == "else" {
                DirectiveKind::Else
            } else {
                DirectiveKind::EndIf
            };
            while pos < line_end && matches!(bytes[pos], b' ' | b'\t') {
                pos += 1;
            }
            let trailing = &source[pos..line_end];
            if trailing.is_empty() || trailing.starts_with("//") {
                Some(Ok(Recognised {
                    directive: match directive_kind {
                        DirectiveKind::Else => Directive::Else,
                        DirectiveKind::EndIf => Directive::EndIf,
                        _ => unreachable!(),
                    },
                    range,
                }))
            } else {
                Some(Err(DirectiveError {
                    keyword: directive_kind,
                    kind: DirectiveErrorKind::UnexpectedTokensAfterDirective,
                    range,
                }))
            }
        }
        // `#nowarn` and `#warnon` are handled by the literal-prefix check
        // above (before the alphabetic-keyword loop), since FCS rule
        // `("#nowarn"|"#warnon") anystring` has no word-boundary requirement
        // and the prefix check needs to fire even for `#nowarn40` / `#warnonx`.
        //
        // `#line`: FCS rule
        //   `"#line" anywhite+ digit+ anywhite* ('@'? "\"" [^"\n\r]+ '"')? anywhite* newline`
        // requires whitespace, a digit run, and a strictly-shaped tail. Bare
        // `#line`, `#line foo`, and `#line 5 garbage` all fail the rule and
        // fall through to ordinary lexing (FCS would too).
        "line" => {
            let after_keyword = pos;
            while pos < line_end && matches!(bytes[pos], b' ' | b'\t') {
                pos += 1;
            }
            if pos == after_keyword {
                return None;
            }
            let digit_start = pos;
            while pos < line_end && bytes[pos].is_ascii_digit() {
                pos += 1;
            }
            if pos == digit_start {
                return None;
            }
            let number = parse_line_number(&source[digit_start..pos]);
            let file = line_directive_tail(bytes, source, pos, line_end)?;
            Some(Ok(Recognised {
                directive: Directive::Line { number, file },
                range,
            }))
        }
        // Not a directive we handle: `#load`, `#r`, `#light`, `#ifdef`
        // partially consumed (keyword would be `"ifdef"` and the word-boundary
        // check passed because the next char is whitespace — but `"ifdef"`
        // falls through here), …
        _ => None,
    }
}

/// Parse and validate the tail of a `#line N` / `# N` directive against FCS's
/// lex rule, capturing the optional filename. `pos` must point immediately
/// past the `digit+` run. The remainder of the line must match FCS's
///   `anywhite* ('@'? "\"" [^"\n\r]+ '"')? anywhite* newline`
/// — i.e. optional whitespace, an optional `@`-prefixed quoted filename,
/// optional whitespace, then end-of-line.
///
/// Returns:
/// - `None` — the tail does not match the FCS rule (e.g. `#line 1 garbage`,
///   `#line 1 "foo` without closing quote, `#line 1 @foo` without quotes, or
///   a line ending in EOF / lone `\r`). The caller falls through to ordinary
///   lexing.
/// - `Some(None)` — the tail matched and carried no filename (`#line 5`).
/// - `Some(Some(file))` — the tail matched with a quoted filename; `file` is
///   the raw text between the quotes (no unescaping; a leading `@` is not part
///   of it).
///
/// The FCS regex anchors the tail with `newline = '\n' | '\r' '\n'`, so a
/// `#line` line that runs straight into EOF, or that ends with a lone `\r`,
/// does *not* match.
fn line_directive_tail(
    bytes: &[u8],
    source: &str,
    mut pos: usize,
    line_end: usize,
) -> Option<Option<String>> {
    while pos < line_end && matches!(bytes[pos], b' ' | b'\t') {
        pos += 1;
    }
    // Optional `'@'? "..."` filename. FCS regex `@?"` requires `@` to be
    // immediately followed by `"` — so if `@` is present but no `"`
    // follows, the rule does not match. We model this with a `saved`
    // rollback rather than a `peek-2` so the structure stays linear.
    let saved = pos;
    if pos < line_end && bytes[pos] == b'@' {
        pos += 1;
    }
    let mut file: Option<String> = None;
    if pos < line_end && bytes[pos] == b'"' {
        pos += 1;
        let body_start = pos;
        while pos < line_end && bytes[pos] != b'"' {
            pos += 1;
        }
        // FCS regex `[^'\n''\r''"']+` requires ≥1 body char, plus a closing
        // `"`. Newlines inside don't apply because we already stop at
        // `line_end`; an unterminated quote (`pos == line_end`) fails here.
        if pos == body_start || pos >= line_end {
            return None;
        }
        // `body_start..pos` lies between two ASCII `"` bytes, so it's a valid
        // UTF-8 boundary regardless of any multibyte content in the filename.
        file = Some(source[body_start..pos].to_string());
        pos += 1; // closing quote
    } else if pos != saved {
        // We consumed a `@` but found no `"` — the FCS regex does not match.
        return None;
    }
    while pos < line_end && matches!(bytes[pos], b' ' | b'\t') {
        pos += 1;
    }
    if pos != line_end {
        return None;
    }
    // FCS rule requires `newline` at the tail: `'\n' | '\r' '\n'`. A lone
    // `\r` (which the outer scan stops at) or EOF (`line_end == bytes.len()`)
    // does not match.
    match bytes.get(line_end) {
        Some(b'\n') => Some(file),
        Some(b'\r') if bytes.get(line_end + 1) == Some(&b'\n') => Some(file),
        _ => None,
    }
}

/// Parse the `digit+` run of a `#line` / `# N` directive into a `u32`. The
/// slice is all ASCII digits (the caller scanned it as such) with no sign, so
/// the only `i32` parse failure is overflow, which we map to `0` — mirroring
/// FCS, whose `int32` conversion errors and falls back to `0` on overflow
/// (`lex.fsl`). We parse as `i32` (not `u32`) so the overflow boundary lands at
/// `i32::MAX`, matching FCS: a value such as `3000000000` (in `i32::MAX + 1 ..=
/// u32::MAX`) overflows `int32` in FCS and so must map to `0` here too. The
/// successful result is non-negative, so the `as u32` cast is exact.
fn parse_line_number(digits: &str) -> u32 {
    digits.parse::<i32>().map(|n| n as u32).unwrap_or(0)
}

/// Extract the warning numbers from the body of a `#nowarn` / `#warnon`
/// directive. `body` is the text *after* the `nowarn` / `warnon` keyword,
/// bounded to the directive's line.
///
/// FCS lexes `("#nowarn"|"#warnon") anystring` as the warn-directive line,
/// then re-parses the lexeme with the regex
///   `( *)#(\S+)(?: +([^ \r\n/;]+))*(?:;;)?( *)(\/\/.*)?$`
/// to extract the arguments (`src/Compiler/SyntaxTree/WarnScopes.fs`). We
/// mirror the observable behaviour on well-formed input:
///
/// - The `\S+` identifier group means the keyword must be followed by
///   whitespace (or end of line) for any arguments to be seen: `#nowarn40`
///   parses to identifier `nowarn40` with *no* arguments. So if the keyword
///   is glued to a non-whitespace byte, there are no numbers.
/// - Arguments are whitespace-separated, with an optional trailing `;;` and an
///   optional `// ...` comment. (FCS's regex only accepts ASCII spaces as the
///   inter-argument separator and disallows a leading tab before the `#`; we
///   treat spaces and tabs uniformly, which only differs from FCS on
///   pathological whitespace that no real directive uses.)
/// - Each argument is parsed by [`parse_warning_number`]; ones that don't
///   parse are dropped (FCS diagnoses them, but the recogniser only reports
///   directive *shape*, so payload parsing is best-effort here).
fn parse_warn_numbers(body: &str) -> Vec<WarningNumber> {
    let after = body.trim_start_matches([' ', '\t']);
    if after.len() == body.len() && !body.is_empty() {
        // The keyword was glued to a non-whitespace byte (`#nowarn40`).
        return Vec::new();
    }
    // Argument characters exclude `/` (FCS arg class `[^ \r\n/;]+`), so the
    // first `//` always terminates the arguments.
    let after = match after.find("//") {
        Some(i) => &after[..i],
        None => after,
    };
    // Drop an optional `;;` terminator (FCS allows it for compatibility).
    let after = after.trim_end_matches([' ', '\t']);
    let after = after.strip_suffix(";;").unwrap_or(after);
    after
        .split_whitespace()
        .filter_map(parse_warning_number)
        .collect()
}

/// Parse a single `#nowarn` / `#warnon` argument into a [`WarningNumber`],
/// mirroring FCS `WarnScopes.getNumber`: strip a `"""..."""` or `"..."` quote
/// wrapping, strip a leading `FS`, then parse with `Int32.TryParse`. Returns
/// `None` for arguments that don't parse.
///
/// # Language-version divergence
///
/// FCS gates both the *unquoted* argument form and the `FS` prefix on the
/// `ParsedHashDirectiveArgumentNonQuotes` language feature, supported from F#
/// 9.0. We don't model language versions, so we always behave as if that
/// feature is on. The default language version is 10.0, so this matches FCS
/// for the default and every langversion >= 9.0; the divergence appears only
/// under an explicitly-selected `<LangVersion>` of 8 or lower.
///
/// FCS's behaviour as a function of whether the feature is on (`f`):
/// 1. Quote-strip `"""..."""` / `"..."` to an inner string (always); a
///    non-quoted argument is accepted only when `f`.
/// 2. An inner string beginning `FS` is accepted (prefix stripped) only when
///    `f` -- this gate applies even to a quoted `"FS40"`.
/// 3. The remainder parses via `Int32.TryParse`.
///
/// So with `f` off, only quoted non-`FS` numbers (`"40"`, `"""40"""`) survive;
/// `40`, `FS40`, and `"FS40"` are all rejected. We accept that whole superset
/// unconditionally. A caller needing langversion-accurate parsing would thread
/// a `bool` for `f` through here and gate steps 1 and 2 on it; recognition
/// (byte consumption) is unaffected either way.
fn parse_warning_number(arg: &str) -> Option<WarningNumber> {
    let unquoted = if let Some(inner) = arg
        .strip_prefix("\"\"\"")
        .and_then(|s| s.strip_suffix("\"\"\""))
    {
        inner
    } else if arg.len() >= 2 && arg.starts_with('"') && arg.ends_with('"') {
        &arg[1..arg.len() - 1]
    } else {
        arg
    };
    let digits = unquoted.strip_prefix("FS").unwrap_or(unquoted);
    digits.parse::<i32>().ok().map(WarningNumber)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ---- example tests: well-formed directives ----

    #[test]
    fn recognises_bare_if() {
        let r = recognise_directive("#if FOO", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 0..7);
    }

    #[test]
    fn recognises_bare_elif() {
        let r = recognise_directive("#elif BAR", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::Elif(Expr::ident("BAR")));
        assert_eq!(r.range, 0..9);
    }

    #[test]
    fn recognises_bare_else() {
        let r = recognise_directive("#else", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::Else);
        assert_eq!(r.range, 0..5);
    }

    #[test]
    fn recognises_bare_endif() {
        let r = recognise_directive("#endif", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::EndIf);
        assert_eq!(r.range, 0..6);
    }

    #[test]
    fn leading_spaces_are_allowed() {
        let r = recognise_directive("   #if FOO", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 0..10);
    }

    #[test]
    fn leading_tabs_are_allowed() {
        let r = recognise_directive("\t#endif", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::EndIf);
        assert_eq!(r.range, 0..7);
    }

    #[test]
    fn mixed_leading_whitespace_is_allowed() {
        let r = recognise_directive(" \t #if FOO", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 0..10);
    }

    #[test]
    fn complex_expression_in_if() {
        let r = recognise_directive("#if (A && B) || !C", 0)
            .unwrap()
            .unwrap();
        let expected = Expr::or(
            Expr::and(Expr::ident("A"), Expr::ident("B")),
            !Expr::ident("C"),
        );
        assert_eq!(r.directive, Directive::If(expected));
    }

    #[test]
    fn trailing_comment_on_if_is_consumed() {
        let r = recognise_directive("#if FOO // why", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 0..14);
    }

    #[test]
    fn trailing_comment_on_else_is_allowed() {
        let r = recognise_directive("#else // foo", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::Else);
        assert_eq!(r.range, 0..12);
    }

    #[test]
    fn trailing_comment_on_endif_is_allowed() {
        let r = recognise_directive("#endif // foo", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::EndIf);
        assert_eq!(r.range, 0..13);
    }

    #[test]
    fn trailing_whitespace_on_else_is_allowed() {
        let r = recognise_directive("#else   ", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::Else);
        assert_eq!(r.range, 0..8);
    }

    #[test]
    fn directive_does_not_consume_newline() {
        let r = recognise_directive("#if FOO\nbar", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 0..7);
    }

    #[test]
    fn directive_at_inner_line_start_is_recognised() {
        let src = "x\n#if FOO";
        let r = recognise_directive(src, 2).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 2..9);
    }

    #[test]
    fn directive_immediately_after_newline_consumes_only_its_line() {
        let src = "#if FOO\n#else\n#endif";
        let r2 = recognise_directive(src, 8).unwrap().unwrap();
        assert_eq!(r2.directive, Directive::Else);
        assert_eq!(r2.range, 8..13);

        let r3 = recognise_directive(src, 14).unwrap().unwrap();
        assert_eq!(r3.directive, Directive::EndIf);
        assert_eq!(r3.range, 14..20);
    }

    // ---- example tests: malformed directives ----

    #[test]
    fn if_with_no_body_is_missing_separator() {
        let err = recognise_directive("#if", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::If);
        assert_eq!(err.kind, DirectiveErrorKind::MissingSeparator);
        assert_eq!(err.range, 0..3);
    }

    #[test]
    fn if_with_only_whitespace_is_missing_expression() {
        let err = recognise_directive("#if   ", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::If);
        assert_eq!(err.kind, DirectiveErrorKind::MissingExpression);
    }

    #[test]
    fn if_with_only_comment_is_missing_expression() {
        let err = recognise_directive("#if  // hmm", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::If);
        assert_eq!(err.kind, DirectiveErrorKind::MissingExpression);
    }

    #[test]
    fn if_without_separating_whitespace_is_missing_separator() {
        // `#if(FOO)` — no space after `#if`. FCS rejects this; we mirror that.
        let err = recognise_directive("#if(FOO)", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::If);
        assert_eq!(err.kind, DirectiveErrorKind::MissingSeparator);
        // Range covers only `#if`, not `(FOO)`.
        assert_eq!(err.range, 0..3);
    }

    #[test]
    fn if_with_bad_expression_is_parse_error() {
        let err = recognise_directive("#if !", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::If);
        assert!(matches!(err.kind, DirectiveErrorKind::ExpressionParse(_)));
    }

    #[test]
    fn if_with_trailing_garbage_is_parse_error() {
        let err = recognise_directive("#if FOO BAR", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::If);
        assert!(matches!(err.kind, DirectiveErrorKind::ExpressionParse(_)));
    }

    #[test]
    fn elif_with_no_body_is_missing_separator() {
        let err = recognise_directive("#elif", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::Elif);
        assert_eq!(err.kind, DirectiveErrorKind::MissingSeparator);
    }

    #[test]
    fn elif_with_bad_expression_is_parse_error_and_reports_elif_keyword() {
        let err = recognise_directive("#elif !", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::Elif);
        assert!(matches!(err.kind, DirectiveErrorKind::ExpressionParse(_)));
    }

    #[test]
    fn else_with_trailing_tokens_is_unexpected() {
        let err = recognise_directive("#else FOO", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::Else);
        assert_eq!(err.kind, DirectiveErrorKind::UnexpectedTokensAfterDirective);
    }

    #[test]
    fn endif_with_trailing_tokens_is_unexpected() {
        let err = recognise_directive("#endif x", 0).unwrap().unwrap_err();
        assert_eq!(err.keyword, DirectiveKind::EndIf);
        assert_eq!(err.kind, DirectiveErrorKind::UnexpectedTokensAfterDirective);
    }

    // ---- CRLF and BOM ----

    #[test]
    fn crlf_line_ending_on_if_is_handled() {
        // The `\r` must not end up inside the expression body.
        let src = "#if FOO\r\nbar";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 0..7);
    }

    #[test]
    fn crlf_line_ending_on_else_is_handled() {
        let src = "#else\r\nrest";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::Else);
        assert_eq!(r.range, 0..5);
    }

    #[test]
    fn crlf_line_ending_on_endif_is_handled() {
        let src = "#endif\r\n";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::EndIf);
        assert_eq!(r.range, 0..6);
    }

    #[test]
    fn crlf_line_ending_with_trailing_comment_is_handled() {
        let src = "#endif // bye\r\n";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::EndIf);
        assert_eq!(r.range, 0..13);
    }

    #[test]
    fn lone_cr_line_ending_on_if_is_handled() {
        // Old-Mac-style line endings: lone `\r`. The Logos lexer treats this
        // as a newline, so this layer must too.
        let src = "#if FOO\rbar";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 0..7);
    }

    #[test]
    fn lone_cr_line_ending_on_endif_is_handled() {
        let src = "#endif\rrest";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::EndIf);
        assert_eq!(r.range, 0..6);
    }

    #[test]
    fn line_start_after_lone_cr_is_anchored() {
        // `line_start = 2` is the byte right after a lone `\r`; the
        // recogniser must accept this as a valid line start.
        let src = "x\r#else";
        let r = recognise_directive(src, 2).unwrap().unwrap();
        assert_eq!(r.directive, Directive::Else);
        assert_eq!(r.range, 2..7);
    }

    #[test]
    fn leading_bom_is_skipped_for_first_line_directive() {
        // `\u{FEFF}` is the UTF-8 BOM, three bytes (EF BB BF).
        let src = "\u{FEFF}#if FOO";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::If(Expr::ident("FOO")));
        assert_eq!(r.range, 0..src.len());
    }

    #[test]
    fn leading_bom_then_whitespace_is_allowed() {
        let src = "\u{FEFF}  #endif";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::EndIf);
    }

    #[test]
    fn bom_only_at_line_start_zero() {
        // A mid-source BOM (after a newline) is not stripped — BOMs are only
        // ever expected at the very start of a file. We don't make claims
        // about this case beyond "doesn't recognise as a directive".
        let src = "x\n\u{FEFF}#if FOO";
        assert!(recognise_directive(src, 2).is_none());
    }

    // ---- example tests: non-directives ----

    #[test]
    fn empty_source_is_not_a_directive() {
        assert!(recognise_directive("", 0).is_none());
    }

    #[test]
    fn blank_line_is_not_a_directive() {
        assert!(recognise_directive("   ", 0).is_none());
    }

    #[test]
    fn regular_code_is_not_a_directive() {
        assert!(recognise_directive("let x = 1", 0).is_none());
    }

    #[test]
    fn lone_hash_is_not_a_directive() {
        assert!(recognise_directive("#", 0).is_none());
    }

    #[test]
    fn hash_with_space_keyword_is_not_a_directive() {
        // `# if FOO` — whitespace between `#` and `if`. FCS's pattern is `"#if"`
        // (no whitespace in between), so this is not a CC directive.
        assert!(recognise_directive("# if FOO", 0).is_none());
    }

    #[test]
    fn load_is_not_a_cc_directive() {
        assert!(recognise_directive("#load \"foo.fsx\"", 0).is_none());
    }

    #[test]
    fn light_is_not_a_cc_directive() {
        assert!(recognise_directive("#light", 0).is_none());
    }

    #[test]
    fn ifdef_is_not_a_cc_directive_word_boundary() {
        // `#ifdef FOO` is not `#if`. FCS routes this to `HASH_IDENT`.
        assert!(recognise_directive("#ifdef FOO", 0).is_none());
    }

    #[test]
    fn elseif_is_not_a_cc_directive_word_boundary() {
        // `#elseif` is not `#else` nor `#elif`.
        assert!(recognise_directive("#elseif FOO", 0).is_none());
    }

    #[test]
    fn endifx_is_not_a_cc_directive_word_boundary() {
        assert!(recognise_directive("#endifx", 0).is_none());
    }

    #[test]
    fn if_followed_by_digit_is_not_a_directive() {
        // `#if5` — digit makes this `#if5` (an identifier), not `#if`.
        assert!(recognise_directive("#if5", 0).is_none());
    }

    #[test]
    fn if_followed_by_underscore_is_not_a_directive() {
        assert!(recognise_directive("#if_FOO", 0).is_none());
    }

    #[test]
    fn if_followed_by_apostrophe_is_not_a_directive() {
        // `'` is an F# identifier-continue character.
        assert!(recognise_directive("#if'", 0).is_none());
    }

    #[test]
    fn if_followed_by_non_ascii_is_not_a_directive() {
        // The byte right after `if` is the lead byte of a multibyte UTF-8
        // letter; treat as identifier-continue.
        assert!(recognise_directive("#ifα FOO", 0).is_none());
    }

    #[test]
    fn block_comment_before_hash_is_not_a_directive() {
        // The line starts with `(`, not whitespace + `#`. FCS rejects
        // `(**)#if` here because the `#` is not the first non-whitespace
        // token; we reject it because the first non-whitespace byte is `(`.
        assert!(recognise_directive("(**)#if FOO", 0).is_none());
    }

    // ---- example tests: offset/anchoring ----

    #[test]
    fn offset_in_middle_of_line_returns_none() {
        // Offset 1 is mid-line (the byte before it is `x`, not `\n`).
        assert!(recognise_directive("x#if FOO", 1).is_none());
    }

    #[test]
    fn offset_past_end_returns_none() {
        assert!(recognise_directive("abc", 100).is_none());
    }

    #[test]
    fn offset_at_end_of_source_returns_none() {
        // Position `source.len()` follows the trailing `\n`, so it's a line
        // start by our definition — but the line is empty, so we still
        // return `None`.
        let src = "x\n";
        assert!(recognise_directive(src, src.len()).is_none());
    }

    // ---- example tests: trivia directives ----

    /// Convenience for asserting a recognised `#nowarn` / `#warnon` payload.
    fn nums(ns: &[i32]) -> Vec<WarningNumber> {
        ns.iter().copied().map(WarningNumber).collect()
    }

    #[test]
    fn nowarn_with_string_arg_is_recognised() {
        let r = recognise_directive("#nowarn \"40\"", 0).unwrap().unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[40])
            }
        );
        assert_eq!(r.range, 0..12);
    }

    #[test]
    fn nowarn_with_numeric_arg_is_recognised() {
        // Unquoted argument — FCS gates this on a language feature; we accept
        // the superset.
        let r = recognise_directive("#nowarn 40", 0).unwrap().unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[40])
            }
        );
    }

    #[test]
    fn nowarn_with_multiple_args_is_recognised() {
        let r = recognise_directive("#nowarn \"40\" \"42\"", 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[40, 42])
            }
        );
    }

    #[test]
    fn nowarn_with_trailing_comment_is_recognised() {
        let r = recognise_directive("#nowarn \"40\" // why", 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[40])
            }
        );
    }

    #[test]
    fn nowarn_strips_fs_prefix_and_leading_zeros() {
        let r = recognise_directive("#nowarn \"FS0057\"", 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[57])
            }
        );
    }

    #[test]
    fn nowarn_triple_quoted_arg() {
        let r = recognise_directive("#nowarn \"\"\"57\"\"\"", 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[57])
            }
        );
    }

    #[test]
    fn nowarn_double_semicolon_terminator() {
        let r = recognise_directive("#nowarn \"57\";;", 0).unwrap().unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[57])
            }
        );
    }

    #[test]
    fn nowarn_invalid_arg_is_dropped() {
        let r = recognise_directive("#nowarn \"abc\"", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::NoWarn { numbers: nums(&[]) });
    }

    #[test]
    fn nowarn_mixed_valid_and_invalid_args() {
        let r = recognise_directive("#nowarn \"57\" abc \"58\"", 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[57, 58])
            }
        );
    }

    #[test]
    fn nowarn_negative_number_via_int32() {
        // FCS uses `Int32.TryParse`, which accepts a leading sign; we mirror
        // that, so the value can be negative even though no real warning is.
        let r = recognise_directive("#nowarn \"-1\"", 0).unwrap().unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[-1])
            }
        );
    }

    #[test]
    fn bare_nowarn_with_no_body_is_recognised() {
        // FCS's `anystring` allows the empty body. We do too — with no numbers.
        let r = recognise_directive("#nowarn", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::NoWarn { numbers: nums(&[]) });
        assert_eq!(r.range, 0..7);
    }

    #[test]
    fn nowarn_leading_whitespace_is_allowed() {
        let r = recognise_directive("    #nowarn 40", 0).unwrap().unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[40])
            }
        );
    }

    #[test]
    fn nowarn_crlf_line_ending_is_handled() {
        let src = "#nowarn 40\r\nrest";
        let r = recognise_directive(src, 0).unwrap().unwrap();
        assert_eq!(
            r.directive,
            Directive::NoWarn {
                numbers: nums(&[40])
            }
        );
        assert_eq!(r.range, 0..10);
    }

    #[test]
    fn warnon_is_recognised() {
        let r = recognise_directive("#warnon \"3218\"", 0).unwrap().unwrap();
        assert_eq!(
            r.directive,
            Directive::WarnOn {
                numbers: nums(&[3218])
            }
        );
    }

    #[test]
    fn bare_warnon_is_recognised() {
        let r = recognise_directive("#warnon", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::WarnOn { numbers: nums(&[]) });
        assert_eq!(r.range, 0..7);
    }

    #[test]
    fn nowarnsomething_is_recognised() {
        // FCS rule is `("#nowarn" | "#warnon") anystring` — there's no
        // word-boundary requirement after the keyword. `#nowarnsomething`
        // matches the rule and is swallowed; FCS reads the lexeme's
        // identifier as `nowarnsomething`, which yields no warning numbers.
        let r = recognise_directive("#nowarnsomething", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::NoWarn { numbers: nums(&[]) });
    }

    #[test]
    fn nowarn_adjacent_digit_is_recognised() {
        // `#nowarn40` (no separating whitespace) matches FCS rule
        // `"#nowarn" anystring`, but FCS reads the identifier as `nowarn40`,
        // so there are no warning numbers.
        let r = recognise_directive("#nowarn40", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::NoWarn { numbers: nums(&[]) });
    }

    #[test]
    fn warnonx_is_recognised() {
        // `#warnonx` (typo / unknown extension) still matches FCS rule
        // `"#warnon" anystring`; FCS surfaces the diagnostic later. No numbers.
        let r = recognise_directive("#warnonx", 0).unwrap().unwrap();
        assert_eq!(r.directive, Directive::WarnOn { numbers: nums(&[]) });
    }

    #[test]
    fn warnoff_is_not_a_directive() {
        // Spelled as `#nowarn`/`#warnon`, never `#warnoff`. FCS would NOT
        // match this — `"#warnon" anystring` requires the literal `warnon`
        // prefix, not `warnoff`.
        assert!(recognise_directive("#warnoff", 0).is_none());
    }

    // All `#line` / bare-numeric tests below explicitly include the trailing
    // `\n` (or `\r\n`): FCS's lex regex anchors the rule to `newline = '\n' |
    // '\r' '\n'`, so EOF and bare `\r` do not satisfy it. The recogniser
    // mirrors that: a `#line` line without a real newline terminator falls
    // through to ordinary lexing.

    /// Convenience for asserting a recognised `#line` payload.
    fn line(number: u32, file: Option<&str>) -> Directive {
        Directive::Line {
            number,
            file: file.map(str::to_string),
        }
    }

    #[test]
    fn line_with_just_number_is_recognised() {
        let r = recognise_directive("#line 5\n", 0).unwrap().unwrap();
        assert_eq!(r.directive, line(5, None));
        // The range covers from the start of the line through the byte
        // *before* the newline — the driver consumes the newline itself
        // via the next line-start jump.
        assert_eq!(r.range, 0..7);
    }

    #[test]
    fn line_with_crlf_terminator_is_recognised() {
        // `\r\n` is also a valid newline per FCS regex.
        let r = recognise_directive("#line 5\r\n", 0).unwrap().unwrap();
        assert_eq!(r.directive, line(5, None));
    }

    #[test]
    fn line_with_number_and_file_is_recognised() {
        let r = recognise_directive("#line 5 \"foo.fs\"\n", 0)
            .unwrap()
            .unwrap();
        assert_eq!(r.directive, line(5, Some("foo.fs")));
    }

    #[test]
    fn line_with_verbatim_file_is_recognised() {
        // FCS's pattern allows an optional `@` prefix on the file name; the
        // `@` is not part of the captured filename (FCS does no unescaping).
        let r = recognise_directive("#line 5 @\"foo.fs\"\n", 0)
            .unwrap()
            .unwrap();
        assert_eq!(r.directive, line(5, Some("foo.fs")));
    }

    #[test]
    fn line_filename_with_spaces_is_captured_verbatim() {
        let r = recognise_directive("#line 5 \"foo bar.fs\"\n", 0)
            .unwrap()
            .unwrap();
        assert_eq!(r.directive, line(5, Some("foo bar.fs")));
    }

    #[test]
    fn line_number_overflow_is_zero() {
        // FCS's `int32` conversion errors and falls back to 0 on overflow; we
        // mirror that by parsing as `i32`.
        let r = recognise_directive("#line 99999999999999999999\n", 0)
            .unwrap()
            .unwrap();
        assert_eq!(r.directive, line(0, None));
    }

    #[test]
    fn line_number_overflows_at_i32_not_u32() {
        // `3000000000` fits a `u32` but overflows `i32`. FCS parses the digit
        // run with `int32`, so it overflows to 0; we must match that rather
        // than exposing the large `u32` value.
        let r = recognise_directive("#line 3000000000\n", 0)
            .unwrap()
            .unwrap();
        assert_eq!(r.directive, line(0, None));
    }

    #[test]
    fn line_number_with_leading_zeros() {
        let r = recognise_directive("#line 007\n", 0).unwrap().unwrap();
        assert_eq!(r.directive, line(7, None));
    }

    #[test]
    fn line_at_eof_without_newline_is_not_a_directive() {
        // FCS regex requires `newline` at the end; EOF doesn't match. The
        // line falls through to ordinary lexing of `#`, `line`, `5`.
        assert!(recognise_directive("#line 5", 0).is_none());
    }

    #[test]
    fn bare_numeric_at_eof_without_newline_is_not_a_directive() {
        // Same EOF check applies to the `# N` alternate.
        assert!(recognise_directive("# 1 \"foo.fs\"", 0).is_none());
    }

    #[test]
    fn line_with_bare_cr_terminator_is_not_a_directive() {
        // FCS regex defines `newline = '\n' | '\r' '\n'` — a lone `\r` is
        // not a newline. Our Logos lexer treats it as one, but the
        // directive recogniser sticks to FCS's stricter definition.
        assert!(recognise_directive("#line 5\rrest", 0).is_none());
    }

    #[test]
    fn bare_line_with_no_body_is_not_a_directive() {
        // FCS requires `"#line" anywhite+ digit+`. Without the digit, no match.
        assert!(recognise_directive("#line\n", 0).is_none());
    }

    #[test]
    fn line_with_non_digit_body_is_not_a_directive() {
        assert!(recognise_directive("#line abc\n", 0).is_none());
    }

    #[test]
    fn line5_without_separator_is_not_a_directive() {
        // `#line5` — no whitespace before the digit. Word-boundary check
        // rejects it; FCS does too (`"#line" anywhite+ digit+` requires
        // the whitespace).
        assert!(recognise_directive("#line5\n", 0).is_none());
    }

    #[test]
    fn bare_numeric_line_directive_with_space_is_recognised() {
        // FCS alternate: `'#' anywhite* digit+ ...`. fslex/fsyacc emit this.
        let r = recognise_directive("# 1 \"fsyacclex.fsl\"\n", 0)
            .unwrap()
            .unwrap();
        assert_eq!(r.directive, line(1, Some("fsyacclex.fsl")));
    }

    #[test]
    fn bare_numeric_line_directive_no_space_is_recognised() {
        // FCS allows zero whitespace between `#` and the digit.
        let r = recognise_directive("#5 \"foo.fs\"\n", 0).unwrap().unwrap();
        assert_eq!(r.directive, line(5, Some("foo.fs")));
    }

    #[test]
    fn bare_numeric_line_directive_tab_separator_is_recognised() {
        let r = recognise_directive("#\t1 \"file\"\n", 0).unwrap().unwrap();
        assert_eq!(r.directive, line(1, Some("file")));
    }

    #[test]
    fn bare_hash_with_non_digit_body_is_not_a_directive() {
        // `# foo` is not a directive — after the optional whitespace, the
        // next byte is not a digit, so the bare-numeric `#line` alternate
        // doesn't match. Other keyword arms can't match either (empty keyword).
        assert!(recognise_directive("# foo\n", 0).is_none());
    }

    #[test]
    fn line_with_garbage_after_number_is_not_a_directive() {
        // FCS rule is `"#line" anywhite+ digit+ anywhite* (@?"..." )? anywhite* newline`
        // — anything other than whitespace / a quoted filename / newline
        // after the digits breaks the match.
        assert!(recognise_directive("#line 1 garbage\n", 0).is_none());
    }

    #[test]
    fn line_with_garbage_after_quoted_file_is_not_a_directive() {
        // After the optional quoted filename, only whitespace + newline is
        // allowed by the FCS regex.
        assert!(recognise_directive("#line 5 \"foo.fs\" trailing\n", 0).is_none());
    }

    #[test]
    fn line_with_unterminated_quoted_file_is_not_a_directive() {
        // FCS regex `[^"\n\r]+ "` requires a closing quote on the same line.
        assert!(recognise_directive("#line 5 \"foo\n", 0).is_none());
    }

    #[test]
    fn line_with_at_sign_no_quote_is_not_a_directive() {
        // FCS regex `@?"...` requires the `@` to be followed by `"`.
        assert!(recognise_directive("#line 5 @foo\n", 0).is_none());
    }

    #[test]
    fn bare_numeric_with_garbage_is_not_a_directive() {
        // Same strict tail applies to the `# N ...` alternate.
        assert!(recognise_directive("# 1 garbage\n", 0).is_none());
    }

    #[test]
    fn line_with_trailing_whitespace_is_recognised() {
        // `anywhite*` after the digit / quoted filename is allowed.
        let r = recognise_directive("#line 5  \t  \n", 0).unwrap().unwrap();
        assert_eq!(r.directive, line(5, None));
    }

    #[test]
    fn trivia_directives_have_no_directive_kind() {
        // Sanity check on the `kind()` API contract: trivia variants
        // return `None` so callers can branch on CC-vs-trivia cleanly.
        assert_eq!(Directive::NoWarn { numbers: vec![] }.kind(), None);
        assert_eq!(Directive::WarnOn { numbers: vec![] }.kind(), None);
        assert_eq!(line(1, None).kind(), None);
        assert!(Directive::NoWarn { numbers: vec![] }.is_trivia());
        assert!(Directive::WarnOn { numbers: vec![] }.is_trivia());
        assert!(line(1, None).is_trivia());
        assert!(!Directive::Else.is_trivia());
        assert!(!Directive::EndIf.is_trivia());
    }

    // ---- property tests ----

    /// Pick from a small alphabet that mixes whitespace (all three line
    /// terminators), identifier chars, CC keyword fragments, and a handful
    /// of punctuation. ASCII-only: non-ASCII is fine but the strategy +
    /// assertions live in byte-space.
    fn arb_source() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\t'),
                Just('\n'),
                Just('\r'),
                Just('#'),
                Just('('),
                Just(')'),
                Just('!'),
                Just('&'),
                Just('|'),
                Just('/'),
                Just('_'),
                Just('\''),
                Just('a'),
                Just('b'),
                Just('i'),
                Just('f'),
                Just('e'),
                Just('l'),
                Just('s'),
                Just('n'),
                Just('d'),
                Just('1'),
                Just('2'),
            ],
            0..60,
        )
        .prop_map(|cs| cs.into_iter().collect())
    }

    /// Same identifier universe as the expression-eval tests.
    fn arb_small_expr() -> impl Strategy<Value = Expr> {
        let leaf = prop_oneof![
            Just(Expr::ident("A")),
            Just(Expr::ident("B")),
            Just(Expr::ident("C")),
            Just(Expr::ident("D")),
        ];
        leaf.prop_recursive(4, 16, 3, |inner| {
            prop_oneof![
                inner.clone().prop_map(|e| !e),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::and(a, b)),
                (inner.clone(), inner).prop_map(|(a, b)| Expr::or(a, b)),
            ]
        })
    }

    proptest! {
        /// Totality: the recogniser never panics on arbitrary input or offset.
        #[test]
        fn recogniser_is_total(s in arb_source(), offset in 0usize..80) {
            let _ = recognise_directive(&s, offset);
        }

        /// When the recogniser returns `Some(_)`, the reported range stays
        /// within the source and does not cross a line terminator. This is
        /// the structural guarantee Stage 4's driver relies on: it can resume
        /// tokenisation at `range.end` without having stepped over a line
        /// boundary, regardless of which line ending the source uses.
        #[test]
        fn range_is_within_line(s in arb_source(), offset in 0usize..80) {
            if let Some(r) = recognise_directive(&s, offset) {
                let range = match &r {
                    Ok(ok) => ok.range.clone(),
                    Err(err) => err.range.clone(),
                };
                prop_assert_eq!(range.start, offset);
                prop_assert!(range.end <= s.len());
                prop_assert!(range.start <= range.end);
                prop_assert!(!s[range.start..range.end].contains('\n'));
                prop_assert!(!s[range.start..range.end].contains('\r'));
            }
        }

        /// Line-anchored: an offset that is neither 0 nor immediately after
        /// a line terminator (`\n` or `\r`) is mid-line, so the recogniser
        /// must return `None`.
        #[test]
        fn non_line_anchored_returns_none(s in arb_source(), offset in 0usize..80) {
            if offset > s.len() || offset == 0 {
                return Ok(());
            }
            let prev = s.as_bytes().get(offset - 1).copied();
            if prev == Some(b'\n') || prev == Some(b'\r') {
                return Ok(());
            }
            prop_assert!(recognise_directive(&s, offset).is_none());
        }

        /// Round-trip: any well-formed `#if <expr>` line parses back to the
        /// same expression. This is the positive correctness signal — without
        /// it, the recogniser could silently rewrite expressions.
        #[test]
        fn well_formed_if_round_trips(e in arb_small_expr()) {
            let s = format!("#if {}", e.to_canonical_string());
            let r = recognise_directive(&s, 0).expect("recognised").expect("ok");
            prop_assert_eq!(r.directive, Directive::If(e));
        }

        /// Same for `#elif`.
        #[test]
        fn well_formed_elif_round_trips(e in arb_small_expr()) {
            let s = format!("#elif {}", e.to_canonical_string());
            let r = recognise_directive(&s, 0).expect("recognised").expect("ok");
            prop_assert_eq!(r.directive, Directive::Elif(e));
        }

        /// `#nowarn` swallows arbitrary trailing text (FCS `anystring`), as
        /// long as the body doesn't extend the keyword. The driver invokes
        /// the recogniser at line starts, so we model "trailing body shape"
        /// by attaching the body via a separator character (space, tab, or
        /// punctuation) that the word-boundary check accepts.
        #[test]
        fn nowarn_swallows_anystring(
            sep in r#"[ \t"!@$%^&*()\-+=:;,<>/?\\\[\]{}|`~]"#,
            rest in r#"[^\n\r]{0,40}"#,
        ) {
            let body = format!("{sep}{rest}");
            // Restrict to ASCII so the assertion is byte-exact.
            if !body.is_ascii() {
                return Ok(());
            }
            let s = format!("#nowarn{body}");
            let r = recognise_directive(&s, 0).expect("recognised").expect("ok");
            let is_nowarn = matches!(r.directive, Directive::NoWarn { .. });
            prop_assert!(is_nowarn);
        }

        /// Same for `#warnon`.
        #[test]
        fn warnon_swallows_anystring(
            sep in r#"[ \t"!@$%^&*()\-+=:;,<>/?\\\[\]{}|`~]"#,
            rest in r#"[^\n\r]{0,40}"#,
        ) {
            let body = format!("{sep}{rest}");
            if !body.is_ascii() {
                return Ok(());
            }
            let s = format!("#warnon{body}");
            let r = recognise_directive(&s, 0).expect("recognised").expect("ok");
            let is_warnon = matches!(r.directive, Directive::WarnOn { .. });
            prop_assert!(is_warnon);
        }

        /// `#line N <FCS-valid tail>` round-trips to `Directive::Line` with the
        /// generated line number and filename. The FCS lex rule is:
        ///   `"#line" anywhite+ digit+ anywhite* ('@'? "\"" [^"\n\r]+ '"')? anywhite* newline`
        /// We generate strictly that shape and assert the recovered payload.
        #[test]
        fn line_with_digits_round_trips(
            n in 0u32..10000,
            ws1_len in 1usize..4,
            ws2_len in 0usize..4,
            ws3_len in 0usize..4,
            file in proptest::option::of((proptest::bool::ANY, r#"[^"\r\n]{1,20}"#)),
        ) {
            let ws1: String = " ".repeat(ws1_len);
            let ws2: String = " ".repeat(ws2_len);
            let ws3: String = " ".repeat(ws3_len);
            let (file_part, expected_file) = match &file {
                Some((at, name)) => {
                    let at = if *at { "@" } else { "" };
                    (format!("{at}\"{name}\""), Some(name.clone()))
                }
                None => (String::new(), None),
            };
            let s = format!("#line{ws1}{n}{ws2}{file_part}{ws3}\n");
            let r = recognise_directive(&s, 0).expect("recognised").expect("ok");
            prop_assert_eq!(r.directive, Directive::Line { number: n, file: expected_file });
        }

        /// Bare-numeric `# N <FCS-valid tail>` form round-trips. Same FCS
        /// regex as `#line`, but the leading-whitespace requirement is
        /// `anywhite*` (not `anywhite+`) so zero spaces is legal.
        #[test]
        fn bare_numeric_line_round_trips(
            ws1_len in 0usize..4,
            n in 0u32..10000,
            ws2_len in 0usize..4,
            ws3_len in 0usize..4,
            file in proptest::option::of((proptest::bool::ANY, r#"[^"\r\n]{1,20}"#)),
        ) {
            let ws1: String = " ".repeat(ws1_len);
            let ws2: String = " ".repeat(ws2_len);
            let ws3: String = " ".repeat(ws3_len);
            let (file_part, expected_file) = match &file {
                Some((at, name)) => {
                    let at = if *at { "@" } else { "" };
                    (format!("{at}\"{name}\""), Some(name.clone()))
                }
                None => (String::new(), None),
            };
            let s = format!("#{ws1}{n}{ws2}{file_part}{ws3}\n");
            let r = recognise_directive(&s, 0).expect("recognised").expect("ok");
            prop_assert_eq!(r.directive, Directive::Line { number: n, file: expected_file });
        }

        /// `#nowarn` / `#warnon` round-trip: a generated list of warning
        /// numbers, each rendered in an arbitrary FCS-accepted form (bare,
        /// quoted, triple-quoted, `FS`-prefixed, leading zeros), separated by
        /// spaces, with an optional `;;` terminator and `// ...` comment, is
        /// recovered exactly. The generator is the oracle: we know which
        /// numbers we wrote.
        #[test]
        fn warn_numbers_round_trip(
            warnon in proptest::bool::ANY,
            nums in prop::collection::vec((0i32..5000, 0usize..6), 1..6),
            semis in proptest::bool::ANY,
            comment in proptest::bool::ANY,
        ) {
            let keyword = if warnon { "warnon" } else { "nowarn" };
            let rendered: Vec<String> = nums
                .iter()
                .map(|&(n, style)| render_warning_arg(n, style))
                .collect();
            let expected: Vec<WarningNumber> =
                nums.iter().map(|&(n, _)| WarningNumber(n)).collect();
            let semis = if semis { ";;" } else { "" };
            let comment = if comment { " // a comment" } else { "" };
            let s = format!("#{keyword} {}{semis}{comment}", rendered.join(" "));
            let r = recognise_directive(&s, 0).expect("recognised").expect("ok");
            let got = match r.directive {
                Directive::NoWarn { numbers } if !warnon => numbers,
                Directive::WarnOn { numbers } if warnon => numbers,
                other => panic!("unexpected directive: {other:?}"),
            };
            prop_assert_eq!(got, expected);
        }
    }

    /// Render a warning number in one of the forms FCS's `getNumber` accepts.
    /// Used by the `warn_numbers_round_trip` property to exercise the quote /
    /// `FS`-prefix / leading-zero handling.
    fn render_warning_arg(n: i32, style: usize) -> String {
        match style % 6 {
            0 => format!("{n}"),
            1 => format!("\"{n}\""),
            2 => format!("\"\"\"{n}\"\"\""),
            3 => format!("FS{n}"),
            4 => format!("\"FS{n}\""),
            // Leading zeros (only well-defined for non-negative values; the
            // generator's range starts at 0).
            _ => format!("\"{n:05}\""),
        }
    }
}
