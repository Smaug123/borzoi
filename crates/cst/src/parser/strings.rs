//! Interpolated- and extended-string productions: fill parsing, the
//! nested-interpolation (FS3373/FS3374) and extended-brace checks, and
//! byte-interp recovery.

use super::*;

impl<'src> Parser<'src> {
    /// `SynExpr.InterpolatedString` — both the bare form `$"hello"` (one
    /// `BeginEnd` fragment, no fills) and the single-fill form
    /// `$"…{ e }…"` (a `Begin` opener, one inner expression, an `End`
    /// closer). The triple-quoted variants `$"""…"""` / `$"""…{ e }…"""`
    /// are accepted via the matching `TripleBeginEnd` / `TripleBegin`
    /// opener variants; their continuation tokens (`Part`/`End`) are
    /// shared with the single-quoted case (the driver's frame stack
    /// carries the style, not the token). Shape:
    /// `INTERP_STRING_EXPR > [INTERP_STRING_FRAGMENT (BeginEnd /
    /// TripleBeginEnd)]` for the bare form; `INTERP_STRING_EXPR >
    /// [INTERP_STRING_FRAGMENT (Begin / TripleBegin), <inner-expr>,
    /// INTERP_STRING_FRAGMENT (End)]` for the single-fill form.
    ///
    /// Emit the FS3377 "a byte string may not be interpolated" diagnostic
    /// and bump a bare byte-interp opener as `lit_kind`. Shared by the
    /// expression path ([`Self::parse_interp_string_expr`]) and the
    /// pattern/const path ([`Self::parse_const_payload`]); the caller
    /// opens the surrounding `CONST_EXPR` / `CONST_PAT` node.
    pub(super) fn recover_byte_interp(
        &mut self,
        lit_kind: SyntaxKind,
        span: std::ops::Range<usize>,
    ) {
        self.errors.push(ParseError {
            message: "a byte string may not be interpolated".to_string(),
            span,
        });
        self.bump_into(lit_kind);
    }

    /// Parse one interpolation fill: the inner `declExpr` plus its optional
    /// `: ident` format qualifier (FCS `declExpr COLON ident %prec
    /// interpolation_fill`, pars.fsy:7059). The `:` and ident are bumped at
    /// the `INTERP_STRING_EXPR` level (not as a child node); the ident is
    /// recovered by `InterpStringExpr::parts()`, which attaches a trailing
    /// top-level `IDENT_TOK` to the fill it follows (the `:` is dropped).
    /// Called once per fill by the fill-loop in
    /// [`Self::parse_interp_string_expr`].
    pub(super) fn parse_interp_fill(&mut self) {
        if self.peek_is_expr_start() {
            self.parse_expr();
        } else if let Some((_, span)) = self.peek().cloned() {
            self.errors.push(ParseError {
                message: "expected expression inside interpolated string fill".to_string(),
                span,
            });
        } else {
            self.errors.push(ParseError {
                message: "unterminated interpolated string".to_string(),
                span: 0..self.source.len(),
            });
        }
        if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Colon)) {
            self.bump_into(SyntaxKind::COLON_TOK);
            if matches!(self.next_non_trivia_raw_at_pos(), Some(Token::Ident(_))) {
                self.bump_into(SyntaxKind::IDENT_TOK);
            } else if let Some((_, span)) = self.peek().cloned() {
                self.errors.push(ParseError {
                    message: "expected identifier after `:` in interpolated string fill"
                        .to_string(),
                    span,
                });
            }
        }
    }

    /// Core of FCS's nested-string check
    /// (`dotnet/fsharp/src/Compiler/lex.fsl:600-699`): a string literal that
    /// appears inside another interp's fill is an error whose code depends on
    /// whether the *inner* literal is triple-quoted and on the
    /// *immediately-enclosing* interp style:
    ///
    /// | enclosing          | inner single/verbatim | inner triple |
    /// |--------------------|-----------------------|--------------|
    /// | single / verbatim  | FS3373                | FS3374       |
    /// | triple             | legal (no error)      | FS3374       |
    ///
    /// A triple-quoted inner literal is *always* FS3374; a single/verbatim
    /// inner is FS3373 only when the enclosing string is also
    /// single/verbatim. Single-in-triple is intentionally diagnostic-free —
    /// it's FCS's own recommended workaround. The check is per-level, hence
    /// the [`Parser::interp_nest`] stack rather than a bool. No-op at top
    /// level (empty stack). FCS recovers the tree in every error case, which
    /// is what our parser already builds. `span` is attached verbatim — the
    /// caller chooses what to point at. Applies to *every* string-literal
    /// form (interp openers via [`Self::check_interp_nesting`], and ordinary
    /// `"…"` / `@"…"` / `"""…"""` literals via [`Self::parse_const_payload`]);
    /// char literals are exempt (FCS-legal).
    pub(super) fn check_nested_string(&mut self, inner_is_triple: bool, span: Range<usize>) {
        let Some(&enclosing) = self.interp_nest.last() else {
            return;
        };
        if inner_is_triple {
            self.errors.push(ParseError {
                message: "Triple quote string literals may not be used in interpolated expressions"
                    .to_string(),
                span,
            });
        } else if enclosing == InterpStyle::SingleOrVerbatim {
            self.errors.push(ParseError {
                message: "Single quote or verbatim string literals may not be used in \
                          interpolated expressions in single quote or verbatim strings"
                    .to_string(),
                span,
            });
        }
    }

    /// [`Self::check_nested_string`] for an interpolated-string opener reached
    /// recursively from a fill. Points at just the `$"` / `$"""` delimiter
    /// prefix rather than the whole opener fragment (which for a bare or
    /// fill-bearing opener runs through the body up to the first `{` or
    /// closer, and would otherwise include text from the *outer* string). The
    /// opener always contains its full delimiter, so `start + len` stays
    /// within `span`.
    pub(super) fn check_interp_nesting(
        &mut self,
        opener: &crate::lexer::InterpKind,
        span: Range<usize>,
    ) {
        use crate::lexer::InterpKind;
        // An extended opener (`$$"""…`, ≥2 `$`) is triple-like for nesting:
        // FCS routes it through the same `lexTripleQuoteInTripleQuote` path
        // (`lex.fsl:613,625`), so it's always FS3374 inside any enclosing
        // interp. Its delimiter prefix is `$`×n + `"""` = `n + 3`.
        let extended_n = match opener {
            InterpKind::ExtendedBegin { n } | InterpKind::ExtendedBeginEnd { n } => Some(*n),
            _ => None,
        };
        let inner_is_triple = extended_n.is_some()
            || matches!(
                opener,
                InterpKind::TripleBegin | InterpKind::TripleBeginEnd { .. }
            );
        let inner_is_verbatim = matches!(
            opener,
            InterpKind::VerbatimBegin | InterpKind::VerbatimBeginEnd { .. }
        );
        // Delimiter prefix length so the diagnostic span covers only the
        // opener: `$$"""` → n+3, `$"""` → 4, `$@"` / `@$"` → 3, `$"` → 2.
        let delim_len = if let Some(n) = extended_n {
            n + 3
        } else if inner_is_triple {
            4
        } else if inner_is_verbatim {
            3
        } else {
            2
        };
        let delim_span = span.start..span.start + delim_len;
        self.check_nested_string(inner_is_triple, delim_span);
    }

    /// Re-scan one extended-interp (`$$"""…`, `n` = delimiter length) fragment
    /// for the two extended-only brace diagnostics. The lexer can't emit
    /// recoverable diagnostics (it returns a single `Result` per token), so —
    /// like FS3373/3374/3377 — these are surfaced here from the fragment text.
    ///
    /// `span` covers exactly one fragment: the opener (`$$"""…{…{` /
    /// `$$"""…"""`), a `Part` (`}…}…{…{`), or the `End` (`}…}…"""`). We strip
    /// the leading delimiter (`$`-run + `"""` for the opener, else the `n`-brace
    /// fill closer) and the trailing delimiter (a closing `"""`, or the trailing
    /// fill-open `{`-run), then inspect the body:
    ///
    /// * **FS1248** — a fill-opening `{`-run of length ≥ `2n` doesn't leave
    ///   enough `$` to take the surplus as content. FCS still opens the fill
    ///   (`lex.fsl:1698`); the run sits at the fragment's trailing edge (a
    ///   shorter mid-body run is plain content and never splits a fragment), so
    ///   we test only that trailing run.
    /// * **FS1249** — a content `}`-run of length ≥ `n` is unmatched
    ///   (`lex.fsl:1733`); the whole run is dropped from the decoded text. One
    ///   diagnostic per maximal offending run.
    /// * **FS1250** — a content `%`-run of length ≥ `2n` exceeds the
    ///   `maxPercents = 2n-1` the `$`-count allows (`lex.fsl:1668`); FCS drops
    ///   the whole run from the decoded text. Shorter `%`-runs are transformed
    ///   (doubled / format-`%`) without error — see
    ///   `tests/all/common/normalised_ast/decode.rs::collapse_extended_interp_body`.
    ///   One
    ///   diagnostic per maximal offending run.
    pub(super) fn check_extended_braces(&mut self, n: usize, span: Range<usize>) {
        let start = span.start;
        let mut diags: Vec<ParseError> = Vec::new();
        {
            let b = self.source[span].as_bytes();
            // Leading delimiter: opener `$`-run + `"""`, else the `n`-brace
            // closer (capped at the actual `}`-run for malformed input).
            let lead = if b.first() == Some(&b'$') {
                b.iter().take_while(|&&c| c == b'$').count() + 3
            } else {
                b.iter().take_while(|&&c| c == b'}').count().min(n)
            };
            // Trailing delimiter: a fill-opening fragment ends with a `{`-run;
            // anything else ends with the closing `"""`.
            let trailing_lbraces = b.iter().rev().take_while(|&&c| c == b'{').count();
            let trail = if trailing_lbraces > 0 {
                // FS1248: the fill still opens, but a run ≥ 2n is over-long.
                if trailing_lbraces >= 2 * n {
                    diags.push(ParseError {
                        message: "The interpolated triple quoted string literal does not start \
                                  with enough '$' characters to allow this many consecutive \
                                  opening braces as content."
                            .to_string(),
                        span: start + (b.len() - trailing_lbraces)..start + b.len(),
                    });
                }
                trailing_lbraces
            } else {
                3.min(b.len().saturating_sub(lead))
            };
            // FS1249: scan the body for maximal `}`-runs of length ≥ n.
            let body_end = b.len().saturating_sub(trail);
            if lead <= body_end {
                let body = &b[lead..body_end];
                let mut i = 0;
                while i < body.len() {
                    if body[i] == b'}' {
                        let run = body[i..].iter().take_while(|&&c| c == b'}').count();
                        if run >= n {
                            diags.push(ParseError {
                                message: "The interpolated string contains unmatched closing \
                                          braces."
                                    .to_string(),
                                span: start + lead + i..start + lead + i + run,
                            });
                        }
                        i += run;
                    } else if body[i] == b'%' {
                        let run = body[i..].iter().take_while(|&&c| c == b'%').count();
                        if run >= 2 * n {
                            diags.push(ParseError {
                                message: "The interpolated triple quoted string literal does not \
                                          start with enough '$' characters to allow this many \
                                          consecutive '%' characters."
                                    .to_string(),
                                span: start + lead + i..start + lead + i + run,
                            });
                        }
                        i += run;
                    } else {
                        i += 1;
                    }
                }
            }
        }
        self.errors.append(&mut diags);
    }

    /// Multi-fill chains (`$"a={x}b={y}"`) are handled by the fill-loop
    /// below: each depth-0 `}` that is not the string closer is lexed as an
    /// `InterpKind::Part` fragment (`}…{`), which splits two consecutive
    /// fills; the chain ends at the `End` fragment (`}…"` / `}…"""`).
    /// A string literal appearing inside a fill — whether an interp opener
    /// (`$"x={ $"y" }"`, recursing through [`Self::parse_expr`] since interp
    /// openers are atomic-expr starts) or an ordinary `"…"` / `@"…"` /
    /// `"""…"""` literal (`$"x={ "y" }"`, parsed by
    /// [`Self::parse_const_payload`]) — is the FS3373/FS3374 nested-string
    /// error. The nested tree is built automatically; the diagnostic is
    /// emitted by [`Self::check_nested_string`] (via
    /// [`Self::check_interp_nesting`] for openers), driven by the
    /// [`Parser::interp_nest`] style stack this method pushes/pops around its
    /// fill-loop. Single-in-triple nesting is intentionally diagnostic-free
    /// (FCS-legal); char literals are exempt.
    pub(super) fn parse_interp_string_expr(&mut self) {
        // Peek the opener once: its kind drives the nesting check, the byte
        // branch, the bare check, and this string's own style.
        let Some((Ok(FilteredToken::Raw(Token::InterpString(kind))), span)) = self.peek().cloned()
        else {
            // The sole dispatch site (`parse_atomic_expr`) only routes here on
            // an interp opener, and nothing advances `pos` in between.
            unreachable!("parse_interp_string_expr called without an interp opener at pos");
        };
        // The nesting check fires before the byte branch so a byte-suffixed
        // nested opener (`$"x={ $"y"B }"`) gets *both* FS3377 and the nesting
        // diagnostic, matching FCS.
        self.check_interp_nesting(&kind, span.clone());
        // FS1245: a `\U` escape > U+10FFFF is rejected in *escape-processing*
        // interp, which is single-quoted only (`$"…"`, `$"…{`). Verbatim,
        // triple, and extended interp don't honour backslash escapes, so they
        // never flag. The opener fragment is scanned here — before the
        // byte-recovery early return — so a bare byte single-interp (`$"\U…"B`)
        // gets FS1245 alongside its FS3377, matching FCS. `Part`/`End`
        // continuations are scanned in the fill-loop below under the same gate.
        let escape_processing = matches!(
            kind,
            crate::lexer::InterpKind::Begin | crate::lexer::InterpKind::BeginEnd { .. }
        );
        if escape_processing {
            self.push_long_unicode_errors(span.clone());
        }
        // Byte suffix on a bare interp string (`$"abc"B` / `$"""abc"""B`):
        // FCS fires FS3377 ("a byte string may not be interpolated") and
        // downgrades the token to a `BYTEARRAY`, recovering
        // `SynConst.Bytes(_, SynByteStringKind.Regular, _)`. We mirror that
        // — emit the diagnostic and build `CONST_EXPR > {BYTE,
        // TRIPLE_BYTE}_STRING_LIT` so the normaliser projects the same
        // `SynConst.Bytes`. The byte fill-bearing form (`$"a={x}"B`) has no
        // clean FCS recovery (it yields `SynExpr.ArbitraryAfterError`), so
        // it falls through to the ordinary interp shape with the diagnostic
        // attached at the byte-tagged `End` closer below.
        if let Some(lit_kind) = byte_interp_lit_kind(&kind) {
            self.builder
                .start_node(FSharpLang::kind_to_raw(SyntaxKind::CONST_EXPR));
            self.recover_byte_interp(lit_kind, span);
            self.builder.finish_node();
            return;
        }
        let is_bare = matches!(
            kind,
            crate::lexer::InterpKind::BeginEnd { .. }
                | crate::lexer::InterpKind::TripleBeginEnd { .. }
                | crate::lexer::InterpKind::VerbatimBeginEnd { .. }
                | crate::lexer::InterpKind::ExtendedBeginEnd { .. }
        );
        // Extended openers (`$$"""…`, ≥2 `$`) carry the delimiter length `n`.
        // We re-scan each fragment for the two extended-only brace diagnostics
        // (FS1248 over-long fill-open `{`-run, FS1249 content `}`-run ≥ n) —
        // the lexer can't emit recoverable diagnostics, so they're parser-side
        // like FS3373/3374/3377. Both the bare and fill-bearing forms can hit
        // FS1249 (a content `}`-run), so the check runs on the opener fragment
        // regardless of `is_bare`.
        let extended_n = match kind {
            crate::lexer::InterpKind::ExtendedBegin { n }
            | crate::lexer::InterpKind::ExtendedBeginEnd { n } => Some(n),
            _ => None,
        };
        if let Some(n) = extended_n {
            self.check_extended_braces(n, span.clone());
        }
        self.builder
            .start_node(FSharpLang::kind_to_raw(SyntaxKind::INTERP_STRING_EXPR));
        self.bump_into(SyntaxKind::INTERP_STRING_FRAGMENT);
        if is_bare {
            self.builder.finish_node();
            return;
        }
        // This string carries at least one fill, so it forms a nesting
        // context: push its style for any inner interp opener reached via the
        // fill-loop's recursion into `parse_expr`, and pop after the loop.
        let my_style = if matches!(
            kind,
            crate::lexer::InterpKind::TripleBegin
                | crate::lexer::InterpKind::TripleBeginEnd { .. }
                | crate::lexer::InterpKind::ExtendedBegin { .. }
                | crate::lexer::InterpKind::ExtendedBeginEnd { .. }
        ) {
            // Extended is triple-like for nesting (FCS routes both through the
            // `lexTripleQuoteInTripleQuote` path).
            InterpStyle::Triple
        } else {
            InterpStyle::SingleOrVerbatim
        };
        self.interp_nest.push(my_style);
        // Fill-loop: parse one fill (`parse_interp_fill`), then look at the
        // fragment that closes it. `End` ends the chain; `Part` (`}…{`)
        // splits this fill from the next, so we bump it and loop. Anything
        // else (a non-fragment token, or EOF from a runaway fill) is a
        // parse error; we still consume what we can so trivia accounting
        // stays sane.
        loop {
            self.parse_interp_fill();
            match self.peek().cloned() {
                Some((
                    Ok(FilteredToken::Raw(Token::InterpString(crate::lexer::InterpKind::End {
                        is_byte,
                    }))),
                    span,
                )) => {
                    // FS1245 in this closer fragment's literal text (`}…"`),
                    // single-quoted interp only.
                    if escape_processing {
                        self.push_long_unicode_errors(span.clone());
                    }
                    // A byte suffix on a fill-bearing interp closer (`}…"B`)
                    // is FS3377 in FCS. There's no clean recovery (FCS
                    // yields `SynExpr.ArbitraryAfterError`), so we keep the
                    // ordinary interp shape and just surface the diagnostic.
                    if is_byte {
                        self.errors.push(ParseError {
                            message: "a byte string may not be interpolated".to_string(),
                            span: span.clone(),
                        });
                    }
                    if let Some(n) = extended_n {
                        self.check_extended_braces(n, span);
                    }
                    self.bump_into(SyntaxKind::INTERP_STRING_FRAGMENT);
                    break;
                }
                Some((
                    Ok(FilteredToken::Raw(Token::InterpString(crate::lexer::InterpKind::Part))),
                    part_span,
                )) => {
                    // FS1245 in this `}…{` part fragment's literal text,
                    // single-quoted interp only.
                    if escape_processing {
                        self.push_long_unicode_errors(part_span.clone());
                    }
                    if let Some(n) = extended_n {
                        self.check_extended_braces(n, part_span);
                    }
                    self.bump_into(SyntaxKind::INTERP_STRING_FRAGMENT);
                }
                Some((_, span)) => {
                    self.errors.push(ParseError {
                        message: "expected closing `}` of interpolated string fill".to_string(),
                        span,
                    });
                    break;
                }
                None => {
                    self.errors.push(ParseError {
                        message: "unterminated interpolated string".to_string(),
                        span: 0..self.source.len(),
                    });
                    break;
                }
            }
        }
        self.interp_nest.pop();
        self.builder.finish_node();
    }
}
