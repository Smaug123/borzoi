//! Token-stream diagnostics for `textDocument/publishDiagnostics`.
//!
//! Pure function over source text + active preprocessor symbols: drive
//! [`borzoi_cst::directives::lex_with_symbols`] so `#if`/`#elif`
//! branches that the symbol set rules out are not lexed (and so cannot
//! produce diagnostics), then translate every reported
//! [`PreprocError`](borzoi_cst::directives::PreprocError) into an LSP
//! `Range` via its `diagnostic_message` / `reporting_span`. Positions are
//! reported in UTF-16 code units, the
//! default LSP position encoding (we don't yet read
//! `general.positionEncodings` from the client during initialise).
//!
//! Both lex errors and structural preprocessor errors are surfaced:
//! malformed directive lines (`#if (`), unbalanced directives
//! (`UnmatchedEndIf`, `OrphanElse`, `OrphanElif`), bad chains
//! (`DoubleElse`, `ElifAfterElse`), and `#if` left unclosed at EOF. The
//! driver already gates these on branch activity — directive-body and
//! chain errors only reach us from active contexts, while orphan /
//! unmatched / unclosed errors are structural and always reported — so
//! the symbol-aware suppression that holds for lex errors holds here too.
//!
//! [`parse_diagnostics`] adds a second producer: structural errors from
//! the recursive-descent parser, run panic-safely and published next to
//! the lexer diagnostics above. The parser is conditional-compilation aware
//! — it consumes the directive driver with the *same* symbol set, so dead
//! `#if` branches and the directive lines are trivia (not squiggles) while a
//! syntax error in the **active** branch is still reported. It does *not*
//! re-report what [`diagnostics_for`] owns: structural directive errors are
//! filtered out of its raw stream (so an orphan `#endif` yields no parser
//! diagnostic at all), and the spurious structural *cascade* the productions
//! emit around an active-branch lex error — a token that fails to lex can't
//! be parsed as an expression — is dropped where it overlaps the lexer's
//! diagnostic, so a lexically-broken token squiggles once, not thrice.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use lsp_types::{Diagnostic, DiagnosticSeverity, Range};

use borzoi_cst::directives::{LineDirectiveStore, lex_with_symbols};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};

use crate::position::offset_to_position;

/// Which top-level F# grammar a buffer is parsed under. A `.fsi` signature file
/// exposes *specifications* (type / `val` / member signatures, no bodies); every
/// other F# extension (`.fs` / `.fsx`) is an implementation file. Selected from
/// the buffer's extension at the dispatch boundary (`server::grouped_for_uri`)
/// and threaded into the parser producer so a `.fsi` is never parsed as an
/// implementation (where a body-less member signature is a spurious
/// "expected `=`" error).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceKind {
    /// `.fs` / `.fsx` — `parse_with_symbols` (the `IMPL_FILE` grammar).
    Implementation,
    /// `.fsi` — `parse_sig_with_symbols` (the `SIG_FILE` grammar).
    Signature,
}

/// Diagnostics destined for a single file. `file == None` is the document's
/// own URI (a same-file `#line` shift, or no governing directive); `Some(s)`
/// is the *verbatim* file string from the governing `#line N "s"` directive,
/// left for the imperative shell to resolve to a URI (Stage 4b of
/// `docs/completed/line-directive-remap-plan.md`). Mirrors FCS regrouping a range onto
/// the directive's file index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiagnostics {
    /// The virtual file the governing directive named, or `None` for the
    /// document's own file.
    pub file: Option<String>,
    /// The diagnostics for `file`, already remapped onto its virtual
    /// coordinates (same-file ones shifted in place; columns untouched).
    pub diagnostics: Vec<Diagnostic>,
}

/// The full diagnostic set for an F# buffer, **partitioned by the virtual
/// file** each diagnostic's start line maps to under the active-branch
/// `#line` directives. Element 0 is always the same-file group
/// (`file: None`); cross-file groups follow in first-appearance order of
/// their verbatim file string, one group per distinct string.
///
/// The preprocessor producer ([`diagnostics_for`]) and the parser producer
/// ([`parse_diagnostics`]) are combined, then each diagnostic governed by a
/// `#line N "f"` is routed into `f`'s group (remapped onto `f`'s virtual
/// coordinates), while same-file and undirected diagnostics stay in the
/// `None` group. The server's F# branch calls this and hands the partition to
/// [`crate::publish::PublishState`], which publishes each group under its own
/// URI.
pub fn grouped_diagnostics(
    text: &str,
    symbols: &HashSet<String>,
    kind: SourceKind,
    lang: LanguageVersion,
) -> Vec<FileDiagnostics> {
    let mut diags = {
        let _s = tracing::info_span!("lex_diagnostics").entered();
        diagnostics_for(text, symbols)
    };
    {
        let _s = tracing::info_span!("parse_diagnostics").entered();
        diags.extend(parse_diagnostics(text, symbols, kind, lang));
    }
    let _s = tracing::info_span!("line_directive_group").entered();
    group_by_line_directives(diags, &line_directive_store(text, symbols))
}

pub fn diagnostics_for(text: &str, symbols: &HashSet<String>) -> Vec<Diagnostic> {
    lex_with_symbols(text, symbols)
        .filter_map(|(tok, span)| {
            let err = match tok {
                Ok(_) => return None,
                Err(e) => e,
            };
            let range = err.reporting_span(span);
            Some(Diagnostic {
                range: Range {
                    start: offset_to_position(text, range.start),
                    end: offset_to_position(text, range.end),
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
pub fn parse_diagnostics(
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
            crate::log_warn!("parser panicked; skipping parser diagnostics for this buffer");
            return Vec::new();
        }
    };

    // Tag each parse diagnostic with its LSP severity, then share the dedup
    // (against the lexer producer's spans) and the offset→position mapping.
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
                start: offset_to_position(text, e.span.start),
                end: offset_to_position(text, e.span.end),
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

#[cfg(test)]
mod tests {
    use super::*;
    use borzoi_cst::language_version::LanguageVersion;
    use lsp_types::Position;

    fn no_symbols() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn clean_input_has_no_diagnostics() {
        assert!(diagnostics_for("let x = 1\nlet y = 2\n", &no_symbols()).is_empty());
        assert!(diagnostics_for("", &no_symbols()).is_empty());
    }

    #[test]
    fn elif_feature_gate_threads_language_version() {
        // The resolved language version reaches the parser through the
        // diagnostics path: under 10.0 (FCS's default) `#elif` draws the FS3350
        // feature diagnostic; under 11.0 / preview it does not.
        let src = "#if FOO\n1\n#elif BAR\n2\n#endif\n";
        let feature_diags = |lang| {
            grouped_diagnostics(src, &no_symbols(), SourceKind::Implementation, lang)
                .iter()
                .flat_map(|g| &g.diagnostics)
                .filter(|d| d.message.contains("#elif preprocessor directive"))
                .count()
        };
        assert_eq!(feature_diags(LanguageVersion::V10_0), 1);
        assert_eq!(feature_diags(LanguageVersion::DEFAULT), 1);
        assert_eq!(feature_diags(LanguageVersion::V11_0), 0);
        assert_eq!(feature_diags(LanguageVersion::Preview), 0);
    }

    #[test]
    fn nullness_feature_gate_threads_language_version() {
        // A *typed-node* feature (nullness) reaches the editor through the same
        // path as the `#elif` trivia feature: under < 9.0 `string | null` draws
        // the FS3350 feature diagnostic; at 9.0 / default (10.0) / preview it does
        // not. No LSP code is nullness-specific — the parser emits it and
        // `is_language_version_diagnostic` keeps it through the overlap dedup.
        let src = "let x : string | null = failwith \"\"\n";
        let feature_diags = |lang| {
            grouped_diagnostics(src, &no_symbols(), SourceKind::Implementation, lang)
                .iter()
                .flat_map(|g| &g.diagnostics)
                .filter(|d| d.message.contains("nullness checking"))
                .count()
        };
        assert_eq!(feature_diags(LanguageVersion::V8_0), 1);
        assert_eq!(feature_diags(LanguageVersion::V9_0), 0);
        assert_eq!(feature_diags(LanguageVersion::DEFAULT), 0);
        assert_eq!(feature_diags(LanguageVersion::Preview), 0);
    }

    #[test]
    fn elif_feature_error_survives_directive_dedup() {
        // A malformed `#elif` under < 11.0 is BOTH a directive-syntax error
        // (which `diagnostics_for` also reports, so the parser's duplicate is
        // deduped) AND an FS3350 feature error (which the lexer path never
        // produces). The feature error must survive the overlap dedup. Regression
        // for the second review of the LSP-langversion wiring.
        let src = "#if FOO\n#elif !\n#endif\n";
        let diags = grouped_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::V10_0,
        );
        let msgs: Vec<&str> = diags
            .iter()
            .flat_map(|g| &g.diagnostics)
            .map(|d| d.message.as_str())
            .collect();
        assert!(
            msgs.iter()
                .any(|m| m.contains("#elif preprocessor directive")),
            "FS3350 feature error must survive the dedup; got {msgs:?}",
        );
    }

    #[test]
    fn unterminated_string_is_reported() {
        let src = "let x = \"oops";
        let diags = diagnostics_for(src, &no_symbols());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "unterminated string literal");
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        // Span covers the opening quote through end-of-input.
        assert_eq!(
            diags[0].range.start,
            Position {
                line: 0,
                character: 8
            }
        );
        assert_eq!(
            diags[0].range.end,
            Position {
                line: 0,
                character: src.encode_utf16().count() as u32
            }
        );
    }

    #[test]
    fn unterminated_block_comment_is_reported() {
        let src = "let x = 1\n(* never closes";
        let diags = diagnostics_for(src, &no_symbols());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "unterminated block comment");
        assert_eq!(
            diags[0].range.start,
            Position {
                line: 1,
                character: 0
            }
        );
    }

    #[test]
    fn closed_block_comment_before_unterminated_one() {
        let src = "(* a *)\n(* b\n";
        let diags = diagnostics_for(src, &no_symbols());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].message, "unterminated block comment");
        assert_eq!(
            diags[0].range.start,
            Position {
                line: 1,
                character: 0,
            }
        );
    }

    #[test]
    fn symbols_select_active_branch() {
        // With FOO defined, the `#else` branch is dead so its
        // unterminated string is not lexed. Mirrors the integration
        // test in `tests/all/ifdef_diagnostics_integration.rs`, but kept
        // here so the unit test suite covers the symbol-aware path
        // even when filesystem fixtures are unavailable.
        let src = "#if FOO\nlet x = 1\n#else\nlet y = \"oops\n#endif\n";
        let with_foo = HashSet::from(["FOO".to_string()]);
        assert!(diagnostics_for(src, &with_foo).is_empty());

        // With FOO undefined the `#else` arm is live, so its unterminated
        // string surfaces. The string also runs to EOF (it swallows the
        // `#endif`), leaving the `#if` unclosed — so an unclosed-`#if`
        // diagnostic correctly rides along. The invariant under test is just
        // that the dead-branch error appears iff the branch is live.
        let diags = diagnostics_for(src, &no_symbols());
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("unterminated string")),
            "{diags:#?}"
        );
    }

    // --- structural preprocessor errors -------------------------------

    #[test]
    fn unmatched_endif_is_reported() {
        let diags = diagnostics_for("#endif\n", &no_symbols());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(
            diags[0].message.contains("#endif"),
            "{:?}",
            diags[0].message
        );
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].source.as_deref(), Some("borzoi"));
    }

    #[test]
    fn orphan_else_is_reported() {
        let diags = diagnostics_for("#else\n", &no_symbols());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(diags[0].message.contains("#else"), "{:?}", diags[0].message);
    }

    #[test]
    fn orphan_elif_is_reported() {
        let diags = diagnostics_for("#elif BAR\n", &no_symbols());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(diags[0].message.contains("#elif"), "{:?}", diags[0].message);
    }

    #[test]
    fn double_else_is_reported() {
        let diags = diagnostics_for("#if FOO\n#else\n#else\n#endif\n", &no_symbols());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(diags[0].message.contains("#else"), "{:?}", diags[0].message);
    }

    #[test]
    fn elif_after_else_is_reported() {
        let diags = diagnostics_for("#if FOO\n#else\n#elif BAR\n#endif\n", &no_symbols());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(diags[0].message.contains("#elif"), "{:?}", diags[0].message);
    }

    #[test]
    fn unclosed_if_highlights_the_if_directive() {
        // The driver pairs `UnclosedIfAtEof` with a zero-width span at EOF;
        // we surface the opening `#if` directive's own span instead, so the
        // squiggle lands on something visible rather than past end-of-file.
        let src = "#if FOO\nlet x = 1\n";
        let diags = diagnostics_for(src, &HashSet::from(["FOO".to_string()]));
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(diags[0].message.contains("#if"), "{:?}", diags[0].message);
        assert_eq!(
            diags[0].range.start,
            Position {
                line: 0,
                character: 0
            }
        );
        assert!(
            diags[0].range.end.line == 0 && diags[0].range.end.character > 0,
            "expected the `#if` line to be highlighted, got {:?}",
            diags[0].range
        );
    }

    #[test]
    fn malformed_directive_condition_is_reported() {
        // `#if` with an unparseable condition in an active context.
        let diags = diagnostics_for("#if )\n#endif\n", &no_symbols());
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("malformed directive")),
            "{diags:#?}"
        );
    }

    #[test]
    fn well_formed_directives_produce_no_structural_errors() {
        // A balanced, well-formed `#if`/`#else`/`#endif` with a clean body
        // must stay silent — the new structural reporting only fires on
        // genuine directive-structure problems.
        let src = "#if FOO\nlet x = 1\n#else\nlet y = 2\n#endif\n";
        assert!(diagnostics_for(src, &HashSet::from(["FOO".to_string()])).is_empty());
        assert!(diagnostics_for(src, &no_symbols()).is_empty());
    }

    #[test]
    fn every_diagnostic_range_is_within_source() {
        // Property: for any input, all diagnostic ranges have offsets in
        // [0, len_in_utf16] for the line they sit on. Splits on all three
        // LSP-recognised line terminators so the check agrees with
        // offset_to_position.
        for src in [
            "",
            "let x = 1",
            "\"unterminated",
            "(* nested (* and unterminated",
            "\u{0000}",
            "À🦀'unclosed",
            "a\rb\"unterminated",
            "first\rsecond\r\nthird\n\"oh",
        ] {
            let lines = split_lsp_lines(src);
            let utf16_lines: Vec<u32> = lines
                .iter()
                .map(|l| l.encode_utf16().count() as u32)
                .collect();
            for d in diagnostics_for(src, &no_symbols()) {
                for p in [d.range.start, d.range.end] {
                    let line = p.line as usize;
                    assert!(line < utf16_lines.len(), "line {line} OOB in {src:?}");
                    assert!(
                        p.character <= utf16_lines[line],
                        "col {} > line len {} in {src:?}",
                        p.character,
                        utf16_lines[line]
                    );
                }
            }
        }
    }

    // --- parse_diagnostics ---------------------------------------------

    use proptest::prelude::*;

    /// `SourceKind` selects the grammar: a `.fsi` type with body-less member
    /// signatures (`member Name : string`) parses **cleanly** under the
    /// *signature* grammar (phase 10.14 slice 3a — `SynMemberSig.Member`),
    /// whereas the *implementation* grammar rejects each body-less member with
    /// "expected `=` after binding pattern" (a member definition needs a body).
    /// A body-less member is the one construct that is signature-valid yet
    /// implementation-invalid, so the diagnostics must differ accordingly.
    #[test]
    fn parse_diagnostics_routes_signature_grammar() {
        let src = "module M\n\
                   type Foo =\n\
                   \x20   /// blah\n\
                   \x20   member Name : string\n\
                   \x20   /// docs\n\
                   \x20   member Other : int\n";

        let sig = parse_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Signature,
            LanguageVersion::Preview,
        );
        assert!(
            sig.is_empty(),
            "the signature grammar accepts body-less member signatures (phase 10.14): {sig:#?}"
        );

        let imp = parse_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert!(
            imp.iter().any(|d| d.message.contains("expected `=`")),
            "the implementation grammar still rejects a body-less member: {imp:#?}"
        );
    }

    /// A genuine structural error (`let` with nothing after `=`) must
    /// surface at least one parser diagnostic, marked ERROR, sourced from
    /// us, with an in-bounds span.
    #[test]
    fn parse_diagnostics_reports_structural_error() {
        let diags = parse_diagnostics(
            "let x =\n",
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert!(!diags.is_empty(), "expected a parser diagnostic, got none");
        for d in &diags {
            assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
            assert_eq!(d.source.as_deref(), Some("borzoi"));
        }
        assert!(all_ranges_in_bounds("let x =\n", &diags));
    }

    /// A `Parse::warnings` entry (here the byte-char trigraph FS1157 warning for
    /// `'\200'B`) is surfaced with WARNING severity, not ERROR — the new
    /// warning channel reaches the LSP.
    #[test]
    fn parse_diagnostics_surfaces_warnings_as_warning_severity() {
        let src = "let b = '\\200'B\n";
        let diags = parse_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        let warn = diags
            .iter()
            .find(|d| d.message.contains("valid byte character literal"))
            .expect("expected the FS1157 byte-char warning");
        assert_eq!(warn.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(warn.source.as_deref(), Some("borzoi"));
        assert!(all_ranges_in_bounds(src, &diags));
    }

    /// Dedup: a *live-branch* lex error is reported once (by the lexer
    /// path). The lexically-broken token can't be parsed as an expression, so
    /// the parser emits a spurious structural cascade at the same span (two
    /// errors, for `let x = "oops`); none of those parser diagnostics may
    /// overlap the lexer's, or the user sees a double squiggle.
    #[test]
    fn parse_diagnostics_dedups_live_lex_error() {
        let src = "let x = \"oops";
        let lexer = diagnostics_for(src, &no_symbols());
        assert_eq!(lexer.len(), 1, "precondition: one lexer lex error");
        let parser = parse_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        for p in &parser {
            for l in &lexer {
                assert!(
                    !ranges_overlap(&p.range, &l.range),
                    "parser diag {:?} overlaps lexer lex error {:?}",
                    p.range,
                    l.range
                );
            }
        }
    }

    /// `diagnostics_for` owns directive errors (here, `#endif` with no
    /// `#if`). The parser is conditional-compilation aware and filters
    /// structural directive errors out of its raw stream, so it reports
    /// **nothing** for the orphan `#endif` — the directive line can't be
    /// double-squiggled. (Pre-C2 the parser re-derived the error from the raw
    /// `#endif` tokens and the overlap-dedup had to drop it; now there is
    /// simply nothing to drop.)
    #[test]
    fn parse_diagnostics_does_not_report_directive_errors() {
        let src = "#endif\n";
        let lexer = diagnostics_for(src, &no_symbols());
        assert_eq!(lexer.len(), 1, "precondition: one preproc diagnostic");
        let parser = parse_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert!(
            parser.is_empty(),
            "parser should report nothing for an orphan #endif; got {parser:?}"
        );
    }

    /// The parser is conditional-compilation aware (`parse_diagnostics`
    /// parses with the project's symbol set), so directive lines are trivia
    /// and the active branch parses cleanly — no squiggle. With `FOO`
    /// defined, `let x = 1` is the active `#if` arm and is well-formed.
    #[test]
    fn parse_diagnostics_does_not_squiggle_directives() {
        let src = "#if FOO\nlet x = 1\n#endif\n";
        let diags = parse_diagnostics(
            src,
            &HashSet::from(["FOO".to_string()]),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert!(
            diags.is_empty(),
            "directive lines / dead branches should no longer squiggle: {diags:?}"
        );
        assert!(all_ranges_in_bounds(src, &diags));
    }

    /// A syntax error in the **active** branch is still reported: with `FOO`
    /// defined the incomplete `let x =` is live code, so the parser squiggles
    /// it. (Regression guard: the parser must parse with the project symbol
    /// set, not an empty one — otherwise this live branch reads as dead.)
    #[test]
    fn parse_diagnostics_reports_active_branch_error() {
        let src = "#if FOO\nlet x =\n#endif\n";
        let diags = parse_diagnostics(
            src,
            &HashSet::from(["FOO".to_string()]),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert!(
            !diags.is_empty(),
            "active-branch syntax error should be reported"
        );
        assert!(all_ranges_in_bounds(src, &diags));
    }

    /// The same incomplete `let` in an *inactive* branch is **not** reported:
    /// with `FOO` undefined it is dead `INACTIVECODE`, which the parser never
    /// lexes.
    #[test]
    fn parse_diagnostics_ignores_inactive_branch_error() {
        let src = "#if FOO\nlet x =\n#endif\n";
        let diags = parse_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert!(
            diags.is_empty(),
            "dead-branch syntax error should not be reported: {diags:?}"
        );
    }

    proptest! {
        /// `diagnostics_for` must never panic and must keep every range in
        /// bounds for arbitrary directive-shaped input. The generator biases
        /// toward `#if`/`#elif`/`#else`/`#endif` lines (interleaved with
        /// short code/garbage lines) so the structural-error paths — orphan
        /// `#else`/`#endif`, double `#else`, unclosed `#if` at EOF — are
        /// actually exercised, not just lex errors.
        #[test]
        fn diagnostics_for_never_panics_and_in_bounds(
            src in "(?s)(#(if|elif|else|endif)[ A-Z]{0,4}\n|[a-z\"() ]{0,6}\n){0,12}"
        ) {
            let diags = diagnostics_for(&src, &no_symbols());
            prop_assert!(all_ranges_in_bounds(&src, &diags));
        }

        /// For arbitrary input the parser path must never panic (the
        /// internal `catch_unwind` guarantees this) and every emitted range
        /// must lie within the source. `(?s)` lets the generator produce
        /// newlines so the line/column bookkeeping is exercised too.
        #[test]
        fn parse_diagnostics_never_panics_and_in_bounds(src in "(?s).{0,80}") {
            let diags = parse_diagnostics(&src, &no_symbols(), SourceKind::Implementation, LanguageVersion::Preview);
            prop_assert!(all_ranges_in_bounds(&src, &diags));
        }

        /// The dedup invariant as a property: no surviving parser diagnostic
        /// overlaps any diagnostic `diagnostics_for` reports, for arbitrary
        /// input.
        #[test]
        fn parse_diagnostics_never_overlap_lexer_errors(src in "(?s).{0,80}") {
            let lexer = diagnostics_for(&src, &no_symbols());
            let parser = parse_diagnostics(&src, &no_symbols(), SourceKind::Implementation, LanguageVersion::Preview);
            for p in &parser {
                for l in &lexer {
                    prop_assert!(!ranges_overlap(&p.range, &l.range));
                }
            }
        }

        /// Same no-overlap invariant, but over directive-shaped input that is
        /// dense with structural preprocessor errors (orphan/unmatched/
        /// unclosed, malformed directives). The parser filters those out of
        /// its raw stream, so this stresses that the filtering plus the
        /// lex-cascade dedup keep every surviving parser diagnostic clear of
        /// the lexer's spans.
        #[test]
        fn parse_diagnostics_never_overlap_directive_errors(
            src in "(?s)(#(if|elif|else|endif)[ A-Z()]{0,4}\n|[a-z\"() ]{0,6}\n){0,12}"
        ) {
            let lexer = diagnostics_for(&src, &no_symbols());
            let parser = parse_diagnostics(&src, &no_symbols(), SourceKind::Implementation, LanguageVersion::Preview);
            for p in &parser {
                for l in &lexer {
                    prop_assert!(!ranges_overlap(&p.range, &l.range));
                }
            }
        }
    }

    // --- #line grouping helpers ---------------------------------------

    use borzoi_cst::directives::LineDirective;

    fn diag_at(start: Position, end: Position) -> Diagnostic {
        Diagnostic {
            range: Range { start, end },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("borzoi".to_string()),
            message: "x".to_string(),
            ..Default::default()
        }
    }

    /// An ascending store, mirroring the real capture invariant.
    fn arb_store() -> impl Strategy<Value = LineDirectiveStore> {
        prop::collection::vec(
            (
                1u32..50,
                0u32..1000,
                prop::option::of(prop_oneof![
                    Just("a.fs".to_string()),
                    Just("b.fs".to_string())
                ]),
            ),
            0..20,
        )
        .prop_map(|rows| {
            let mut store = LineDirectiveStore::new();
            let mut generated_line = 0u32;
            for (gap, virtual_line, file) in rows {
                store.push(LineDirective {
                    generated_line,
                    virtual_line,
                    file,
                });
                generated_line = generated_line.saturating_add(gap);
            }
            store
        })
    }

    /// Valid diagnostic range: `end.line >= start.line`.
    fn arb_diag() -> impl Strategy<Value = Diagnostic> {
        (0u32..2000, 0u32..50, 0u32..200, 0u32..200).prop_map(|(start_line, height, sc, ec)| {
            diag_at(
                Position {
                    line: start_line,
                    character: sc,
                },
                Position {
                    line: start_line + height,
                    character: ec,
                },
            )
        })
    }

    // --- #line grouping (grouped_diagnostics / group_by_line_directives) ---

    fn ld(generated_line: u32, virtual_line: u32, file: Option<&str>) -> LineDirective {
        LineDirective {
            generated_line,
            virtual_line,
            file: file.map(str::to_string),
        }
    }

    fn store_of(directives: Vec<LineDirective>) -> LineDirectiveStore {
        let mut store = LineDirectiveStore::new();
        for d in directives {
            store.push(d);
        }
        store
    }

    /// An empty store yields a single same-file group holding every input
    /// diagnostic unchanged.
    #[test]
    fn group_empty_store_is_single_same_file_group() {
        let input = vec![
            diag_at(
                Position {
                    line: 1,
                    character: 0,
                },
                Position {
                    line: 2,
                    character: 3,
                },
            ),
            diag_at(
                Position {
                    line: 9,
                    character: 0,
                },
                Position {
                    line: 9,
                    character: 1,
                },
            ),
        ];
        let groups = group_by_line_directives(input.clone(), &LineDirectiveStore::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].file, None);
        assert_eq!(groups[0].diagnostics, input);
    }

    /// A cross-file directive routes a following diagnostic into its named
    /// group, remapped onto the virtual line; the same-file group stays
    /// present (element 0) but empty.
    #[test]
    fn group_routes_cross_file_to_named_group() {
        let store = store_of(vec![ld(0, 100, Some("other.fs"))]);
        let groups = group_by_line_directives(
            vec![diag_at(
                Position {
                    line: 5,
                    character: 3,
                },
                Position {
                    line: 8,
                    character: 7,
                },
            )],
            &store,
        );
        assert_eq!(groups.len(), 2, "{groups:#?}");
        assert_eq!(groups[0].file, None);
        assert!(groups[0].diagnostics.is_empty(), "{groups:#?}");
        assert_eq!(groups[1].file.as_deref(), Some("other.fs"));
        // remap(5) = 5 + 100 − 0 − 2 = 103; delta 98 applied to both ends.
        assert_eq!(
            groups[1].diagnostics[0].range.start,
            Position {
                line: 103,
                character: 3
            }
        );
        assert_eq!(
            groups[1].diagnostics[0].range.end,
            Position {
                line: 106,
                character: 7
            }
        );
    }

    /// Cross-file groups follow in *first-appearance* order of their file
    /// string, not sorted: the diagnostic mapping to `b.fs` is emitted first,
    /// so `b.fs`'s group precedes `a.fs`'s.
    #[test]
    fn group_orders_cross_files_by_first_appearance() {
        let store = store_of(vec![ld(0, 10, Some("a.fs")), ld(5, 20, Some("b.fs"))]);
        let groups = group_by_line_directives(
            vec![
                diag_at(
                    Position {
                        line: 7,
                        character: 0,
                    },
                    Position {
                        line: 7,
                        character: 1,
                    },
                ),
                diag_at(
                    Position {
                        line: 3,
                        character: 0,
                    },
                    Position {
                        line: 3,
                        character: 1,
                    },
                ),
            ],
            &store,
        );
        assert_eq!(groups[0].file, None);
        assert_eq!(groups[1].file.as_deref(), Some("b.fs"), "{groups:#?}");
        assert_eq!(groups[2].file.as_deref(), Some("a.fs"));
    }

    /// Two directives naming the same file string collapse into one group.
    #[test]
    fn group_dedups_same_file_string() {
        let store = store_of(vec![ld(0, 10, Some("a.fs")), ld(5, 50, Some("a.fs"))]);
        let groups = group_by_line_directives(
            vec![
                diag_at(
                    Position {
                        line: 3,
                        character: 0,
                    },
                    Position {
                        line: 3,
                        character: 1,
                    },
                ),
                diag_at(
                    Position {
                        line: 7,
                        character: 0,
                    },
                    Position {
                        line: 7,
                        character: 1,
                    },
                ),
            ],
            &store,
        );
        assert_eq!(groups.len(), 2, "{groups:#?}");
        assert_eq!(groups[1].file.as_deref(), Some("a.fs"));
        assert_eq!(groups[1].diagnostics.len(), 2);
    }

    /// A same-file `#line N` and a cross-file `#line N "v.fs"` split a pair of
    /// diagnostics into the None group and the `v.fs` group respectively.
    #[test]
    fn group_splits_same_and_cross_file() {
        let store = store_of(vec![ld(0, 100, None), ld(5, 200, Some("v.fs"))]);
        let groups = group_by_line_directives(
            vec![
                diag_at(
                    Position {
                        line: 2,
                        character: 0,
                    },
                    Position {
                        line: 2,
                        character: 4,
                    },
                ),
                diag_at(
                    Position {
                        line: 7,
                        character: 0,
                    },
                    Position {
                        line: 7,
                        character: 4,
                    },
                ),
            ],
            &store,
        );
        // None group: line 2 governed by `#line 100` @0 → 2 + 100 − 0 − 2 = 100.
        assert_eq!(groups[0].file, None);
        assert_eq!(groups[0].diagnostics.len(), 1, "{groups:#?}");
        assert_eq!(groups[0].diagnostics[0].range.start.line, 100);
        // v.fs group: line 7 governed by `#line 200 "v.fs"` @5 → 7 + 200 − 5 − 2 = 200.
        assert_eq!(groups[1].file.as_deref(), Some("v.fs"));
        assert_eq!(groups[1].diagnostics[0].range.start.line, 200);
    }

    /// End-to-end: a real `#line N "other.fs"` before a lexer error routes
    /// that error into the `other.fs` group, remapped onto its virtual line.
    #[test]
    fn grouped_diagnostics_routes_cross_file_directive() {
        let src = "#line 100 \"other.fs\"\nlet x = \"oops\n";
        let groups = grouped_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert_eq!(groups[0].file, None, "{groups:#?}");
        let cross = groups
            .iter()
            .find(|g| g.file.as_deref() == Some("other.fs"))
            .expect("expected an other.fs group");
        let d = cross
            .diagnostics
            .iter()
            .find(|d| d.message.contains("unterminated string"))
            .expect("expected the unterminated-string diag in the cross-file group");
        // generated line 1; `#line 100` @0 ⇒ 1 + 100 − 0 − 2 = 99.
        assert_eq!(d.range.start.line, 99, "{groups:#?}");
    }

    /// End-to-end: a same-file `#line N` keeps the diagnostic in the None
    /// group; no cross-file group is created.
    #[test]
    fn grouped_diagnostics_same_file_stays_in_none_group() {
        let src = "#line 100\nlet x = \"oops\n";
        let groups = grouped_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert!(groups.iter().all(|g| g.file.is_none()), "{groups:#?}");
        let d = groups[0]
            .diagnostics
            .iter()
            .find(|d| d.message.contains("unterminated string"))
            .expect("expected the unterminated-string diag in the None group");
        assert_eq!(d.range.start.line, 99);
    }

    /// End-to-end partition: every diagnostic the two producers emit appears
    /// exactly once across the groups (message + columns preserved), nothing
    /// dropped or duplicated.
    #[test]
    fn grouped_diagnostics_partitions_all_producer_output() {
        let src = "#line 50 \"a.fs\"\nlet x = \"oops\n";
        let groups = grouped_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        let mut raw = diagnostics_for(src, &no_symbols());
        raw.extend(parse_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        ));
        let total: usize = groups.iter().map(|g| g.diagnostics.len()).sum();
        assert_eq!(total, raw.len(), "groups={groups:#?}\nraw={raw:#?}");
        let proj = |d: &Diagnostic| {
            (
                d.message.clone(),
                d.range.start.character,
                d.range.end.character,
            )
        };
        let mut got: Vec<_> = groups
            .iter()
            .flat_map(|g| g.diagnostics.iter().map(proj))
            .collect();
        let mut want: Vec<_> = raw.iter().map(proj).collect();
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }

    /// End-to-end: with no `#line`, grouping yields a single None group whose
    /// diagnostics are exactly the two producers concatenated — the grouping
    /// layer is a true no-op on the directive-free path.
    #[test]
    fn grouped_diagnostics_without_directive_is_the_two_producers() {
        let src = "let x = \"oops\n";
        let groups = grouped_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].file, None);
        let mut expected = diagnostics_for(src, &no_symbols());
        expected.extend(parse_diagnostics(
            src,
            &no_symbols(),
            SourceKind::Implementation,
            LanguageVersion::Preview,
        ));
        assert_eq!(groups[0].diagnostics, expected);
    }

    proptest! {
        /// Partition: the multiset of diagnostics (message + columns + height,
        /// all invariant under the line remap) across every group equals the
        /// input multiset. Nothing is dropped or duplicated.
        #[test]
        fn group_partition_preserves_diags_modulo_line(
            store in arb_store(),
            diags in prop::collection::vec(arb_diag(), 0..10),
        ) {
            let groups = group_by_line_directives(diags.clone(), &store);
            let proj = |d: &Diagnostic| {
                (
                    d.message.clone(),
                    d.range.start.character,
                    d.range.end.character,
                    d.range.end.line - d.range.start.line,
                )
            };
            let mut got: Vec<_> = groups.iter().flat_map(|g| g.diagnostics.iter().map(proj)).collect();
            let mut want: Vec<_> = diags.iter().map(proj).collect();
            got.sort();
            want.sort();
            prop_assert_eq!(got, want);
        }

        /// The same-file group is always element 0, and the cross-file groups
        /// that follow have distinct `Some(_)` file strings.
        #[test]
        fn group_none_first_and_files_distinct(
            store in arb_store(),
            diags in prop::collection::vec(arb_diag(), 0..10),
        ) {
            let groups = group_by_line_directives(diags, &store);
            prop_assert_eq!(groups[0].file.clone(), None);
            for g in &groups[1..] {
                prop_assert!(g.file.is_some());
            }
            let mut files: Vec<_> = groups[1..].iter().map(|g| g.file.clone()).collect();
            let len = files.len();
            files.sort();
            files.dedup();
            prop_assert_eq!(files.len(), len);
        }

        /// Single-diagnostic placement, the precise oracle: a lone diagnostic
        /// with original start line `s` lands in the group named by
        /// `store.remap(s)` (None group when no directive precedes or the
        /// directive is same-file), at line `store.remap(s).line` (or `s`),
        /// with height, both columns, and all metadata preserved. This pins
        /// 4a's routing to the *same* `remap` call `apply_line_directives`
        /// makes — only the destination differs.
        #[test]
        fn group_single_diag_placement(store in arb_store(), diag in arb_diag()) {
            let s = diag.range.start.line;
            let height = diag.range.end.line - diag.range.start.line;
            let groups = group_by_line_directives(vec![diag.clone()], &store);
            let total: usize = groups.iter().map(|g| g.diagnostics.len()).sum();
            prop_assert_eq!(total, 1);
            let (file, placed) = groups
                .iter()
                .find_map(|g| g.diagnostics.first().map(|d| (g.file.clone(), d.clone())))
                .unwrap();
            match store.remap(s) {
                None => {
                    prop_assert_eq!(file, None);
                    prop_assert_eq!(placed.range.start.line, s);
                }
                Some(r) => {
                    prop_assert_eq!(file, r.file.clone());
                    prop_assert_eq!(placed.range.start.line, r.line);
                }
            }
            prop_assert_eq!(placed.range.end.line - placed.range.start.line, height);
            prop_assert_eq!(placed.range.start.character, diag.range.start.character);
            prop_assert_eq!(placed.range.end.character, diag.range.end.character);
            prop_assert_eq!(placed.message, diag.message);
            prop_assert_eq!(placed.severity, diag.severity);
            prop_assert_eq!(placed.source, diag.source);
            prop_assert_eq!(groups[0].file.clone(), None);
        }
    }

    /// True iff every diagnostic range sits within `src` (line in range,
    /// column within that line's UTF-16 length). Mirrors the check in
    /// `every_diagnostic_range_is_within_source`.
    fn all_ranges_in_bounds(src: &str, diags: &[Diagnostic]) -> bool {
        let lines = split_lsp_lines(src);
        let utf16_lines: Vec<u32> = lines
            .iter()
            .map(|l| l.encode_utf16().count() as u32)
            .collect();
        diags.iter().all(|d| {
            [d.range.start, d.range.end].iter().all(|p| {
                let line = p.line as usize;
                line < utf16_lines.len() && p.character <= utf16_lines[line]
            })
        })
    }

    /// Half-open overlap of two LSP ranges (touching at a point is *not*
    /// overlap, matching the byte-span dedup rule).
    fn ranges_overlap(a: &Range, b: &Range) -> bool {
        let lt = |p: Position, q: Position| (p.line, p.character) < (q.line, q.character);
        lt(a.start, b.end) && lt(b.start, a.end)
    }

    /// Like `str::lines`, but treats lone `\r` as a line terminator too,
    /// matching the lexer's newline regex and LSP's spec.
    fn split_lsp_lines(text: &str) -> Vec<&str> {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' => {
                    out.push(&text[start..i]);
                    i += if bytes.get(i + 1) == Some(&b'\n') {
                        2
                    } else {
                        1
                    };
                    start = i;
                }
                b'\n' => {
                    out.push(&text[start..i]);
                    i += 1;
                    start = i;
                }
                _ => i += 1,
            }
        }
        out.push(&text[start..]);
        out
    }
}
