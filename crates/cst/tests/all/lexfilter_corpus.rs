//! Corpus-wide diagnostic for the LexFilter port.
//!
//! Walks the corpus rooted at `BORZOI_CORPUS` (the `fsharp-src` flake
//! input under `nix develop`), runs our `filter` on every ASCII-only F# file whose
//! raw lex succeeds, and compares the resulting token stream against FCS's
//! parser-facing stream (via `fcs-dump tokens-filtered-batch`).
//!
//! This is intentionally *not* an assertion: the port is incomplete, so most
//! files diverge somewhere. The output is a histogram of divergence categories
//! — bucketed by `(rust_kind, fcs_kind, fcs_preceding_kind)` — and a small
//! sample of file paths per bucket. The dominant buckets point at the next
//! FCS arm worth porting.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use borzoi_cst::directives::{PreprocError, lex_with_symbols};
use borzoi_cst::lexfilter::{FilteredToken, Virtual, filter};
use serde::Deserialize;

use crate::common::{
    NormalisedToken, corpus_root, fcs_tokens_filtered_batch, filtered_kind_name, parse_fcs_dump,
};

/// Same list `tests/all/corpus.rs` uses: paths containing any of these are
/// deliberately-malformed fixtures and should be skipped wholesale.
const EXPECTED_FAILURE_SUBSTRINGS: &[&str] = &[
    "/E_",
    "/Diagnostics/NONTERM/interactiveExprOrDefinitionsTerminator06",
    "/SyntaxTree/Expression/Id ",
    "/SyntaxTree/Expression/Unfinished escaped ident ",
    "/SyntaxTree/Type/Type 10.fs",
    "/ConditionalCompilation/InComment01.fs",
    "/ConditionalCompilation/InStringLiteral03.fs",
];

fn is_expected_failure(path: &Path) -> bool {
    let p = path.to_string_lossy();
    EXPECTED_FAILURE_SUBSTRINGS.iter().any(|s| p.contains(s))
}

// Ignored by default: the FCS side (`fcs-dump tokens-filtered-batch`) takes
// ~1.65 s per file on average — multiple hours over the full F# corpus.
// The Rust filter itself runs in ~1.4 ms per file (debug), so this is
// purely the cost of dotnet/FCS, not our code. The test is a diagnostic that
// bucketises divergences to point at the next FCS arm worth porting; it is
// not an assertion (see the module docstring). Run with:
//   cargo test --test all lexfilter_corpus:: -- --ignored --nocapture
#[ignore = "FCS-bound diagnostic, hours on the full corpus"]
#[test]
fn diff_filtered_corpus() {
    let root = corpus_root();

    // ---- 1. Collect & pre-filter the file list ----
    let mut all_files = Vec::new();
    collect_fsharp_files(&root, &mut all_files);
    let total_seen = all_files.len();

    let mut payloads: Vec<FilePayload> = Vec::with_capacity(all_files.len());
    let mut skipped_allowlist = 0;
    let mut skipped_non_utf8 = 0;
    let mut skipped_non_ascii = 0;
    let mut skipped_lex_error = 0;

    for path in all_files {
        if is_expected_failure(&path) {
            skipped_allowlist += 1;
            continue;
        }
        let source = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                skipped_non_utf8 += 1;
                continue;
            }
        };
        // `parse_fcs_dump`'s LineIndex maps FCS's UTF-16-code-unit columns
        // to byte offsets by adding `col` to the line-start byte. That only
        // round-trips for ASCII; multibyte files would silently produce
        // misaligned spans and noise the divergence histogram.
        if !source.is_ascii() {
            skipped_non_ascii += 1;
            continue;
        }
        let symbols: HashSet<String> = HashSet::new();
        if lex_with_symbols(&source, &symbols).any(|(t, _)| t.is_err()) {
            skipped_lex_error += 1;
            continue;
        }
        payloads.push(FilePayload { path, source });
    }

    eprintln!(
        "corpus: {total_seen} files seen / {kept} kept \
         (skipped: {skipped_allowlist} allow-list, {skipped_non_utf8} non-UTF-8, \
         {skipped_non_ascii} non-ASCII, {skipped_lex_error} lex error)",
        kept = payloads.len(),
    );
    assert!(!payloads.is_empty(), "no usable files under {root:?}");

    // ---- 2. Run FCS batch & compare ----
    // Lock-step through the shared batch child, as `parser_corpus_diff` does: one
    // path per request, one JSONL line back. That child is bounded and
    // self-healing (a wedged or crashed oracle is killed, respawned, and the
    // request retried), which the hand-rolled streaming driver this replaces was
    // not: it read a wedged child's stdout forever. The `dotnet build` gate goes
    // with it — the shared harness builds the oracle on first use, under its own
    // deadline.
    let mut histogram: HashMap<DivergenceKey, BucketStats> = HashMap::new();
    let mut compared = 0usize;
    let mut matched = 0usize;
    let mut diverged = 0usize;
    let mut batch_errors = 0usize;
    let mut filter_panicked = 0usize;

    for payload in &payloads {
        let line = fcs_tokens_filtered_batch(&payload.path);

        // A malformed response is a broken oracle, not a divergence: the request
        // was answered with something that isn't a record. The streaming driver
        // this replaces logged and skipped it, and caught the resulting hole with
        // a records-sent-vs-received accounting assertion; under lock-step there
        // is no count left to reconcile, so a skip here would silently shrink the
        // corpus the sweep claims to have covered. Fail instead, naming the file.
        let envelope: BatchEnvelope = serde_json::from_str(&line).unwrap_or_else(|e| {
            panic!(
                "malformed JSONL from fcs-dump for {}: {e}\nline: {line}",
                payload.path.display()
            )
        });
        if envelope.error.is_some() {
            batch_errors += 1;
            continue;
        }

        let fcs_tokens = parse_fcs_dump(&line, &payload.source);
        let rust_tokens = match run_rust_filter(&payload.source) {
            Ok(t) => t,
            Err(_) => {
                filter_panicked += 1;
                continue;
            }
        };
        // Count only files where both sides produced a token stream — so
        // the later `compared > 0` assertion catches a corpus where every
        // Rust filter call panicked too.
        compared += 1;

        if rust_tokens == fcs_tokens {
            matched += 1;
            continue;
        }
        diverged += 1;
        record_first_divergence(&mut histogram, &payload.path, &rust_tokens, &fcs_tokens);
    }

    eprintln!(
        "\ncompared {compared} files: {matched} match, {diverged} diverge \
         ({batch_errors} FCS errors, {filter_panicked} Rust panics)"
    );

    // The diagnostic is only meaningful if FCS actually produced an oracle for at
    // least some payloads. The old streaming driver needed three silent-pass traps
    // here; lock-step requests retire two of them. A request that cannot be
    // answered now panics inside the batch child (rather than quietly truncating
    // the stream), and an unparseable answer panics above (rather than vanishing
    // from the counts), so every payload lands in exactly one bucket by
    // construction and there is no sent-vs-received total left to reconcile. What
    // remains is the third: every record could be a `{Path, Error}` envelope (FCS
    // erroring on all files, e.g. an SDK mismatch), leaving nothing compared.
    assert!(
        compared > 0,
        "fcs-dump returned a per-file error for every one of {} payloads; \
         no oracle data to compare against",
        payloads.len(),
    );

    if diverged == 0 {
        return;
    }

    // ---- 3. Report top divergence buckets ----
    let mut buckets: Vec<(DivergenceKey, BucketStats)> = histogram.into_iter().collect();
    buckets.sort_by_key(|b| std::cmp::Reverse(b.1.count));

    let top_n = 25usize;
    eprintln!(
        "\ntop {} divergence buckets (rust_kind × fcs_kind, with FCS context):",
        top_n.min(buckets.len())
    );
    for (key, stats) in buckets.iter().take(top_n) {
        eprintln!(
            "  {:>5}  rust={:<28} fcs={:<24} after fcs=[{}]",
            stats.count,
            key.rust_kind.as_deref().unwrap_or("(EOF)"),
            key.fcs_kind.as_deref().unwrap_or("(EOF)"),
            key.fcs_prev.as_deref().unwrap_or("(start)"),
        );
        for sample in stats.samples.iter().take(3) {
            eprintln!("           e.g. {}", sample.display());
        }
    }

    let total_buckets = buckets.len();
    if total_buckets > top_n {
        let tail: usize = buckets.iter().skip(top_n).map(|(_, s)| s.count).sum();
        eprintln!(
            "  ... {} more buckets covering {} files",
            total_buckets - top_n,
            tail
        );
    }
}

// ============================================================================
// Plumbing
// ============================================================================

struct FilePayload {
    path: PathBuf,
    source: String,
}

#[derive(Deserialize)]
struct BatchEnvelope {
    #[serde(rename = "Error", default)]
    error: Option<String>,
}

fn run_rust_filter(source: &str) -> Result<Vec<NormalisedToken>, ()> {
    // The directive driver swallows `#nowarn` / `#warnon` / `#line` and skips
    // inactive `#if` arms, mirroring FCS's `Compiling | SkipTrivia` lexer
    // flags — so the raw stream we feed `filter` is already free of the
    // hash-directive lines that used to dominate the divergence histogram.
    let symbols: HashSet<String> = HashSet::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let raw = lex_with_symbols(source, &symbols).map(|(tok, span)| {
            // The pre-filter rejects any file with a preproc error, so by the
            // time we reach this function the only error shape that can occur
            // is `PreprocError::Lex` — which we unwrap back to `LexError` so
            // `filter` (which takes the raw lexer's error type) is happy.
            let tok = tok.map_err(|e| match e {
                PreprocError::Lex(e) => e,
                other => unreachable!("non-Lex PreprocError after pre-filter: {other:?}"),
            });
            (tok, span)
        });
        filter(source, raw)
            // Drop `BlockEnd` and `AndBang`: both are absent from FCS's public
            // token stream, for *different* reasons. `OAND_BANG` has no
            // `FSharpTokenKind` arm (→ `None`). `OBLOCKEND` *does* map to a real
            // kind (`OffsideBlockEnd`), but FCS's outer LexFilter wrapper
            // swallows every one and re-inserts `OBLOCKEND_COMING_SOON`/`_IS_HERE`
            // tokens (→ `None`) in its place, so a real block end never reaches
            // the public tokenizer. Either way `tokens-filtered` shows neither
            // (see `common::assert_filtered_streams_match` for the full
            // mechanism, and `lexfilter_diff::block_end` for the placement pins
            // this drop would otherwise leave unverified).
            .filter(|(tok, _)| {
                !matches!(
                    tok,
                    Ok(FilteredToken::Virtual(Virtual::BlockEnd | Virtual::AndBang))
                )
            })
            .filter_map(|(tok, span)| {
                let tok = tok.ok()?;
                Some(NormalisedToken {
                    kind: filtered_kind_name(&tok),
                    start: span.start,
                    end: span.end,
                })
            })
            .collect::<Vec<_>>()
    }));
    result.map_err(|_| ())
}

// ============================================================================
// Divergence bucketing
// ============================================================================

#[derive(PartialEq, Eq, Hash)]
struct DivergenceKey {
    rust_kind: Option<String>,
    fcs_kind: Option<String>,
    /// FCS token immediately before the divergence — disambiguates buckets
    /// like "OffsideBlockEnd vs (nothing) after `OffsideRightBlockEnd`" from
    /// "OffsideBlockEnd vs (nothing) after `Equals`".
    fcs_prev: Option<String>,
}

struct BucketStats {
    count: usize,
    samples: Vec<DivergenceSample>,
}

struct DivergenceSample {
    path: PathBuf,
    idx: usize,
    fcs_span: (usize, usize),
}

impl DivergenceSample {
    fn display(&self) -> String {
        format!(
            "{} @ token {} (fcs span {}..{})",
            self.path.display(),
            self.idx,
            self.fcs_span.0,
            self.fcs_span.1,
        )
    }
}

fn record_first_divergence(
    histogram: &mut HashMap<DivergenceKey, BucketStats>,
    path: &Path,
    rust: &[NormalisedToken],
    fcs: &[NormalisedToken],
) {
    let len = rust.len().max(fcs.len());
    let mut first_diff = None;
    for i in 0..len {
        let r = rust.get(i);
        let f = fcs.get(i);
        if r != f {
            first_diff = Some(i);
            break;
        }
    }
    let Some(idx) = first_diff else { return };

    let rust_kind = rust.get(idx).map(|t| t.kind.clone());
    let fcs_kind = fcs.get(idx).map(|t| t.kind.clone());
    let fcs_prev = if idx == 0 {
        None
    } else {
        fcs.get(idx - 1).map(|t| t.kind.clone())
    };
    let fcs_span = fcs
        .get(idx)
        .map(|t| (t.start, t.end))
        .or_else(|| rust.get(idx).map(|t| (t.start, t.end)))
        .unwrap_or((0, 0));

    let key = DivergenceKey {
        rust_kind,
        fcs_kind,
        fcs_prev,
    };
    let entry = histogram.entry(key).or_insert_with(|| BucketStats {
        count: 0,
        samples: Vec::new(),
    });
    entry.count += 1;
    if entry.samples.len() < 5 {
        entry.samples.push(DivergenceSample {
            path: path.to_path_buf(),
            idx,
            fcs_span,
        });
    }
}

// ============================================================================
// File walker (copied from tests/all/corpus.rs — same scope, no third-party dep)
// ============================================================================

fn collect_fsharp_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some(".git" | "target" | "artifacts" | "bin" | "obj")
            ) {
                continue;
            }
            collect_fsharp_files(&path, out);
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("fs" | "fsi" | "fsx")
        ) {
            out.push(path);
        }
    }
}
