//! The syntactic diagnostics as the server computed them before Stage 5 of
//! `docs/incremental-lsp-plan.md`: three lexes of their own beside the parse
//! (the preprocessor's errors, the overlap spans the parser's are deduplicated
//! against, and the `#line` directives). Kept verbatim as the reference
//! `borzoi::diagnostics::syntactic_diagnostics`, which reads all three from
//! the parse's own lex, is held to (`syntactic_diagnostics_diff`,
//! `parser_corpus_sweep`).

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use borzoi::diagnostics::{FileDiagnostics, SourceKind};
use borzoi::position::LineIndex;
use borzoi_cst::directives::{LineDirectiveStore, lex_with_symbols};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use lsp_types::{Diagnostic, DiagnosticSeverity, Range};

pub fn grouped_diagnostics(
    text: &str,
    symbols: &HashSet<String>,
    kind: SourceKind,
    lang: LanguageVersion,
) -> Vec<FileDiagnostics> {
    let mut diags = { diagnostics_for(text, symbols) };
    {
        diags.extend(parse_diagnostics(text, symbols, kind, lang));
    }
    group_by_line_directives(diags, &line_directive_store(text, symbols))
}

fn diagnostics_for(text: &str, symbols: &HashSet<String>) -> Vec<Diagnostic> {
    let lines = LineIndex::new(text);
    lex_with_symbols(text, symbols)
        .filter_map(|(tok, span)| {
            let err = match tok {
                Ok(_) => return None,
                Err(e) => e,
            };
            let range = err.reporting_span(span);
            Some(Diagnostic {
                range: Range {
                    start: lines.position(range.start),
                    end: lines.position(range.end),
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("borzoi".to_string()),
                message: err.diagnostic_message(),
                ..Default::default()
            })
        })
        .collect()
}

/// Structural diagnostics from the recursive-descent parser, published
/// alongside [`diagnostics_for`].
///
/// The parser is run panic-safely. It is still very incomplete, so on
/// arbitrary input one of its internal invariant guards may fire; a panic
/// degrades to "no parser diagnostics for this buffer" — the lexer
/// diagnostics still stand — rather than taking the server down.
///
/// Errors [`diagnostics_for`] already owns are *not* re-reported here. The
/// parser parses the same active branches (it shares `symbols`), so dead
/// branches and directive lines are trivia, never parser errors, and
/// structural directive errors are filtered out of its raw stream — those
/// belong solely to `diagnostics_for`. What still needs suppressing is the
/// parser's *spurious structural cascade* around an **active-branch lex
/// error**: a token that fails to lex can't be parsed as an expression, so
/// the productions emit one or more structural errors at the lex-error span.
/// Any parser error whose span overlaps a span `diagnostics_for` reports is
/// dropped, so the user sees the lexer's single diagnostic there, not the
/// cascade. (`reported_spans` covers every preproc-error span, not just lex
/// ones: the parser now *also* surfaces directive errors — at the same
/// [`reporting_span`](borzoi_cst::directives::PreprocError::reporting_span)
/// this producer uses — so they fall in this
/// overlap-dedup and the user sees `diagnostics_for`'s single squiggle, not
/// two.) The one parser diagnostic *exempt* from the dedup is a language-version
/// feature error (FS3350, e.g. `#elif` below F# 11): `diagnostics_for` never
/// produces it, so it must survive even when it shares a directive's span with a
/// malformed-condition error — see `is_language_version_diagnostic`.
fn parse_diagnostics(
    text: &str,
    symbols: &HashSet<String>,
    kind: SourceKind,
    lang: LanguageVersion,
) -> Vec<Diagnostic> {
    let reported_spans: Vec<std::ops::Range<usize>> = lex_with_symbols(text, symbols)
        .filter_map(|(tok, span)| match tok {
            Ok(_) => None,
            Err(e) => Some(e.reporting_span(span)),
        })
        .collect();

    // `.fsi` parses under the signature grammar; `.fs` / `.fsx` under the
    // implementation grammar. Both share the lexer/lex-filter pipeline; the kind
    // only picks the top-level production. `lang` gates version-specific
    // features (e.g. `#elif`) the same way for both.
    let file_kind = match kind {
        SourceKind::Implementation => FileKind::Impl,
        SourceKind::Signature => FileKind::Sig,
    };
    let opts = ParseOptions {
        file_kind,
        symbols,
        lang,
    };
    let parsed = match catch_unwind(AssertUnwindSafe(|| parse_with_options(text, opts))) {
        Ok(parsed) => parsed,
        Err(_) => {
            return Vec::new();
        }
    };

    // Tag each parse diagnostic with its LSP severity, then share the dedup
    // (against the lexer producer's spans) and the offset→position mapping.
    let lines = LineIndex::new(text);
    parsed
        .errors
        .into_iter()
        .map(|e| (e, DiagnosticSeverity::ERROR))
        .chain(
            parsed
                .warnings
                .into_iter()
                .map(|e| (e, DiagnosticSeverity::WARNING)),
        )
        // The overlap dedup drops the parser's *duplicates* of the lexer/
        // preprocessor producer (the structural cascade around an active-branch
        // lex error, and the parser's copy of a directive-syntax error). A
        // language-version feature diagnostic (FS3350) is neither — `diagnostics_for`
        // never produces it — so it must survive even when it shares a directive's
        // span with a malformed-condition error (e.g. `#elif !` under < F# 11).
        .filter(|(e, _)| {
            is_language_version_diagnostic(&e.message)
                || !reported_spans.iter().any(|l| spans_overlap(&e.span, l))
        })
        .map(|(e, severity)| Diagnostic {
            range: Range {
                start: lines.position(e.span.start),
                end: lines.position(e.span.end),
            },
            severity: Some(severity),
            source: Some("borzoi".to_string()),
            message: e.message,
            ..Default::default()
        })
        .collect()
}

/// Whether `message` is a language-version feature diagnostic (FCS FS3350): a
/// construct the parser flagged solely because the pinned `<LangVersion>`
/// predates it (e.g. `#elif` below F# 11). The lexer/preprocessor producer
/// ([`diagnostics_for`]) never emits these, so they must be exempt from the
/// overlap dedup — otherwise one sharing a directive's span with a
/// malformed-condition error would be silently dropped. Matched on the
/// feature-independent FS3350 template tail rather than a per-feature string, so
/// it covers every gated feature, not just `#elif`.
fn is_language_version_diagnostic(message: &str) -> bool {
    message.contains("Please use language version")
}

/// Half-open intersection of two byte ranges. A zero-width span `[n, n)`
/// (the parser emits these for "expected …" placeholders) is treated as
/// overlapping `[a, b)` when `a <= n <= b`, so a placeholder sitting
/// exactly at the start or end of a lex error still dedups.
fn spans_overlap(a: &std::ops::Range<usize>, b: &std::ops::Range<usize>) -> bool {
    if a.start == a.end {
        return b.start <= a.start && a.start <= b.end;
    }
    if b.start == b.end {
        return a.start <= b.start && b.start <= a.end;
    }
    a.start < b.end && b.start < a.end
}

/// Drain a preprocessor pass purely to recover the active-branch `#line`
/// directives. The store is only complete once the driver iterator is fully
/// consumed (see [`borzoi_cst::directives::Driver::line_directives`]),
/// so we run it to exhaustion and discard the tokens — they were already
/// produced by [`diagnostics_for`].
fn line_directive_store(text: &str, symbols: &HashSet<String>) -> LineDirectiveStore {
    let mut driver = lex_with_symbols(text, symbols);
    for _ in driver.by_ref() {}
    driver.line_directives().clone()
}

/// Partition diagnostics by the virtual file their **start** line maps to
/// under `store`. The same-file group (`file: None`) is always element 0 —
/// it collects diagnostics governed by no directive (kept at generated
/// coordinates) and by a `#line N` with no file (shifted in place). Each
/// subsequent group collects the diagnostics governed by a `#line N "f"`,
/// remapped onto `f`'s virtual coordinates; groups appear in first-appearance
/// order of `f`, deduplicated by the verbatim string.
///
/// Every diagnostic is anchored on its start line: the governing directive's
/// delta shifts *both* ends (height preserved) and columns are never touched,
/// mirroring FCS's `range.ApplyLineDirectives` (one `xOffset` to start and
/// end). A cross-file diagnostic is *routed to its file's group* rather than
/// remapped in place. An empty `store` yields a single same-file group
/// holding every diagnostic unchanged.
fn group_by_line_directives(
    diags: Vec<Diagnostic>,
    store: &LineDirectiveStore,
) -> Vec<FileDiagnostics> {
    let mut same_file: Vec<Diagnostic> = Vec::new();
    // Cross-file groups in first-appearance order. A linear scan to find the
    // matching group is fine: a buffer names only a handful of virtual files.
    let mut cross: Vec<FileDiagnostics> = Vec::new();
    for mut d in diags {
        let Some(remapped) = store.remap(d.range.start.line) else {
            same_file.push(d);
            continue;
        };
        let delta = i64::from(remapped.line) - i64::from(d.range.start.line);
        d.range.start.line = remapped.line;
        d.range.end.line = (i64::from(d.range.end.line) + delta).max(0) as u32;
        match remapped.file {
            None => same_file.push(d),
            Some(file) => match cross.iter_mut().find(|g| g.file.as_deref() == Some(&file)) {
                Some(group) => group.diagnostics.push(d),
                None => cross.push(FileDiagnostics {
                    file: Some(file),
                    diagnostics: vec![d],
                }),
            },
        }
    }
    let mut out = Vec::with_capacity(cross.len() + 1);
    out.push(FileDiagnostics {
        file: None,
        diagnostics: same_file,
    });
    out.extend(cross);
    out
}
