//! Differential AST sweep: our parser vs FCS over the whole corpus.
//!
//! The sibling `parser_corpus.rs` sweep proves the parser *round-trips* real
//! F# and tracks how much it parses without errors — but "no errors" is not
//! "correct". This test closes that gap: for every `.fs` / `.fsi` / `.fsx`
//! file under `BORZOI_CORPUS` where **our** parser is clean, it normalises
//! both our AST and FCS's `ParsedInput` to the shared model (the same projection
//! the hand-written `parser_diff_*` tests use) and asserts they agree.
//!
//! FCS's ASTs come from the shared `fcs-dump ast-batch` request/response child
//! (paths in on stdin, one `ParsedInput` JSON per line out), so the ~150 ms .NET
//! startup and `FSharpChecker` construction are amortised over the corpus
//! instead of paid per file. The shared batch wrapper bounds each request with a
//! timeout and respawns the oracle on a wedge/crash, so one silent FCS deadlock
//! cannot hang the whole sweep.
//!
//! The normalisers are closed-world: they `panic!` on any construct they don't
//! model yet (and the parser/normaliser grow together, phase by phase). Over
//! real source that is the common case, so each side is wrapped in
//! `catch_unwind` and a file only reaches the equality check when **both**
//! sides normalise. Every file lands in exactly one [`Bucket`]:
//!
//! * **match** — both normalise and are equal, and the audited broad ranges
//!   (modules and declarations) equal FCS's too. The headline coverage.
//! * **range-divergent** — the shapes match but an audited range differs.
//! * **ast-divergent** — both normalise but differ. A real signal: a parser
//!   bug, or a normaliser asymmetry.
//! * **we-accept-fcs-rejects** — our parser is clean but FCS reports
//!   `ParseHadErrors`: a parser acceptance gap (typically a negative `E_*`
//!   fixture). Kept apart from the AST comparison so a recovery AST cannot be
//!   counted as a match.
//! * the rest — either side panicked or does not model a construct, both
//!   parsers rejected, our parse had errors while FCS was clean, FCS failed on
//!   the file, or the source is not UTF-8. Expected, and pinned all the same.
//!
//! # The manifest
//!
//! The corpus is content-addressed (pinned in `flake.nix`) and both parsers are
//! deterministic, so every file's bucket is a fixed fact, and the sweep checks
//! it **exactly** against `tests/manifests/parser_corpus_diff.txt`: one line per
//! file outside the match bucket, keyed by corpus-relative path, and one summary
//! line counting the matches. Since every non-match is listed by path, a file
//! moving into or out of the match bucket moves a listed line as well as the
//! count. A new acceptance gap, a lost match, a new range divergence and a
//! newly fixed one all fail the run alike; an intended movement is acknowledged
//! by regenerating the manifest and committing the diff:
//!
//! ```text
//! BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-cst --test all parser_corpus_diff:: -- --ignored
//! ```
//!
//! Regeneration cannot bless a broken oracle: a record without
//! `ParseHadErrors`, a mismatched response path or script classification, a
//! range audit that panics, or a file the sweep fails to bucket are assertions
//! that run first.
//!
//! Both sides use FCS's service-parser implicit symbol set for the file kind:
//! `COMPILED` + `EDITING` for compiled `.fs`/`.fsi`, and `INTERACTIVE` +
//! `EDITING` for `.fsx` scripts. That keeps `#if` branches aligned instead of
//! diverging on symbol-set mismatch.
//!
//! `#[ignore]`d like `parser_corpus.rs`: it parses the corpus twice (us + FCS)
//! and is slow. Run with
//! `cargo test -p borzoi-cst --test all parser_corpus_diff:: -- --ignored`
//! under `nix develop` (which sets `BORZOI_CORPUS`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use borzoi_cst::parser::{Parse, parse_sig_with_symbols, parse_with_symbols};
use borzoi_oracle_harness::manifest::Manifest;
use serde::Deserialize;

use crate::common::catch_unwind_silent;
use crate::common::corpus_manifest::{
    check_manifest, regenerate_ignored, relative_key, summary_entry,
};
use crate::common::normalised_ast::{normalise_fcs_dump, normalise_parse};
use crate::common::recovery::grade;
use crate::common::{
    ast_ranges_match, collect_fsharp_corpus_files, corpus_root, fcs_ast_batch, read_corpus_source,
};

/// How many paths per bucket to print for investigation.
const DIVERGENCE_SAMPLE: usize = 40;

/// Lightweight view of one `ast-batch` JSONL record: enough to correlate and to
/// detect a per-file FCS failure and FCS's parse accept/reject bit. The
/// heavyweight `ParseTree` is left to [`normalise_fcs_dump`], which re-reads the
/// same line.
#[derive(Deserialize)]
struct BatchMeta {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Error")]
    error: Option<String>,
    #[serde(rename = "ParseHadErrors")]
    parse_had_errors: Option<bool>,
    #[serde(rename = "IsScript")]
    is_script: Option<bool>,
}

/// The one outcome each corpus file gets. The manifest names it by
/// [`Bucket::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bucket {
    /// Shapes and audited ranges both equal FCS's.
    Match,
    /// Shapes equal, an audited range differs.
    RangeDivergent,
    /// Shapes equal, and the range audit itself panicked: the range oracle is
    /// not total over the structurally matched corpus. Asserted absent.
    RangeAuditPanicked,
    /// Both sides normalise, to different models.
    AstDivergent,
    /// Our parse is clean; FCS reports `ParseHadErrors`.
    WeAcceptFcsRejects,
    /// Both parsers report errors.
    BothReject,
    /// FCS is clean; our parse has errors.
    WeRejectFcsAccepts,
    /// Our parser panicked.
    OurParsePanicked,
    /// Our normaliser does not model a construct in our (clean) tree.
    OurNormaliserUnmodelled,
    /// FCS's tree did not normalise: a construct `from_fcs` does not model, or
    /// a tree deeper than `serde_json`'s recursion limit.
    FcsNormaliserUnmodelled,
    /// `ast-batch` reported a per-file FCS failure (`{Path, Error}`).
    FcsError,
    /// The `ast-batch` record did not parse as [`BatchMeta`].
    FcsRecordMalformed,
    /// A non-error record without `ParseHadErrors`. Asserted absent.
    FcsMissingParseStatus,
    /// The source is not UTF-8, and our parser takes `&str`.
    NonUtf8,
}

impl Bucket {
    fn name(self) -> &'static str {
        match self {
            Bucket::Match => "match",
            Bucket::RangeDivergent => "range-divergent",
            Bucket::RangeAuditPanicked => "range-audit-panicked",
            Bucket::AstDivergent => "ast-divergent",
            Bucket::WeAcceptFcsRejects => "we-accept-fcs-rejects",
            Bucket::BothReject => "both-reject",
            Bucket::WeRejectFcsAccepts => "we-reject-fcs-accepts",
            Bucket::OurParsePanicked => "our-parse-panicked",
            Bucket::OurNormaliserUnmodelled => "our-normaliser-unmodelled",
            Bucket::FcsNormaliserUnmodelled => "fcs-normaliser-unmodelled",
            Bucket::FcsError => "fcs-error",
            Bucket::FcsRecordMalformed => "fcs-record-malformed",
            Bucket::FcsMissingParseStatus => "fcs-missing-parse-status",
            Bucket::NonUtf8 => "non-utf8",
        }
    }
}

/// Parse `src` with our parser for `path`'s file kind, or `None` if it panics.
/// `parser_corpus.rs` asserts the raw parser panics on no corpus file under the
/// empty symbol set; catch here too, so one file can't abort the sweep.
fn parse_ours(path: &Path, src: &str, symbols: &HashSet<String>) -> Option<Parse> {
    let is_sig = is_signature_path(path);
    catch_unwind_silent(|| {
        if is_sig {
            parse_sig_with_symbols(src, symbols)
        } else {
            parse_with_symbols(src, symbols)
        }
    })
    .ok()
}

/// One file's outcome: its bucket, the range audit's message for a range
/// divergence (for the printed sample), and — when either parser reported an
/// error — the recovered-tree verdict (`common::recovery`), which the manifest
/// pins after the bucket name.
struct Outcome {
    bucket: Bucket,
    range_message: Option<String>,
    recovery: Option<Recovery>,
}

/// A recovered-tree verdict as the manifest records it, plus the first
/// divergence for the printed sample.
struct Recovery {
    token: String,
    divergence: Option<String>,
}

impl Outcome {
    fn bucket(bucket: Bucket) -> Self {
        Outcome {
            bucket,
            range_message: None,
            recovery: None,
        }
    }

    fn recovered(bucket: Bucket, parse: &Parse, line: &str, src: &str) -> Self {
        let recovery = match catch_unwind_silent(|| grade(parse, line, src)) {
            Ok(Ok(v)) => Recovery {
                token: v.token(),
                divergence: v.first_divergence,
            },
            // FCS's tree is deeper than `serde_json` reads; nothing to grade.
            Ok(Err(_)) => Recovery {
                token: "fcs-unreadable".to_string(),
                divergence: None,
            },
            Err(_) => panic!("grading a recovered tree panicked"),
        };
        Outcome {
            bucket,
            range_message: None,
            recovery: Some(recovery),
        }
    }
}

/// Classify one UTF-8 corpus file against its `ast-batch` record `line`.
fn classify(
    path: &Path,
    src: &str,
    line: &str,
    compiled_symbols: &HashSet<String>,
    script_symbols: &HashSet<String>,
) -> Outcome {
    // `BatchMeta` ignores the heavy `ParseTree` field, so this stays cheap
    // and — unlike a full `Value` parse — does not trip the recursion limit
    // on deep files.
    let Ok(meta) = serde_json::from_str::<BatchMeta>(line) else {
        return Outcome::bucket(Bucket::FcsRecordMalformed);
    };
    assert_eq!(
        Path::new(&meta.path),
        path,
        "fcs-dump ast-batch response path did not match request"
    );
    if meta.error.is_some() {
        return Outcome::bucket(Bucket::FcsError);
    }
    assert_eq!(
        meta.is_script,
        Some(is_script_path(path)),
        "fcs-dump ast-batch script classification did not match request"
    );
    let Some(fcs_had_errors) = meta.parse_had_errors else {
        return Outcome::bucket(Bucket::FcsMissingParseStatus);
    };
    let symbols = if is_script_path(path) {
        script_symbols
    } else {
        compiled_symbols
    };

    if fcs_had_errors {
        let Some(ours) = parse_ours(path, src, symbols) else {
            return Outcome::bucket(Bucket::OurParsePanicked);
        };
        let bucket = if ours.errors.is_empty() {
            Bucket::WeAcceptFcsRejects
        } else {
            Bucket::BothReject
        };
        return Outcome::recovered(bucket, &ours, line, src);
    }

    // Normalise the FCS side *first*. Its internal full `Value` parse fails
    // (caught here) on trees deeper than `serde_json`'s default recursion
    // limit, so a pathologically deep file is skipped before our own
    // recursive-descent parser ever runs on it — keeping that parser, which
    // we do not run under a guard stack, off stack-overflow-deep input.
    let Ok(fcs_norm) = catch_unwind_silent(|| normalise_fcs_dump(line)) else {
        return Outcome::bucket(Bucket::FcsNormaliserUnmodelled);
    };

    // Our side: parse the same source. We only compare where *we* are
    // clean and FCS is clean — an error tree is not a meaningful thing to
    // diff.
    let Some(ours) = parse_ours(path, src, symbols) else {
        return Outcome::bucket(Bucket::OurParsePanicked);
    };
    if !ours.errors.is_empty() {
        return Outcome::recovered(Bucket::WeRejectFcsAccepts, &ours, line, src);
    }
    let Ok(ours_norm) = catch_unwind_silent(|| normalise_parse(&ours)) else {
        return Outcome::bucket(Bucket::OurNormaliserUnmodelled);
    };

    if ours_norm != fcs_norm {
        return Outcome::bucket(Bucket::AstDivergent);
    }
    match catch_unwind_silent(|| ast_ranges_match(&ours, line, src)) {
        Ok(Ok(())) => Outcome::bucket(Bucket::Match),
        Ok(Err(message)) => Outcome {
            bucket: Bucket::RangeDivergent,
            range_message: Some(message),
            recovery: None,
        },
        Err(_) => Outcome::bucket(Bucket::RangeAuditPanicked),
    }
}

#[test]
#[ignore = "full-corpus differential parse (us + FCS); run with --ignored under nix develop"]
fn parser_matches_fcs_over_corpus() {
    let root = corpus_root();

    let files = collect_fsharp_corpus_files(&root)
        .unwrap_or_else(|err| panic!("walk F# corpus under {}: {err}", root.display()));
    assert!(!files.is_empty(), "no .fs/.fsi/.fsx files under {root:?}");

    eprintln!(
        "differentially parsing {} files under {}",
        files.len(),
        root.display()
    );

    // `FSharpChecker.ParseFile` (the service parser `ast-batch` uses)
    // implicitly defines `COMPILED` + `EDITING` for a compiled `.fs`/`.fsi`,
    // and `INTERACTIVE` + `EDITING` for a `.fsx` script. Match that exact set;
    // otherwise conditional-compilation branches diverge purely on symbol-set
    // mismatch.
    let compiled_symbols: HashSet<String> = ["COMPILED", "EDITING"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let script_symbols: HashSet<String> = ["INTERACTIVE", "EDITING"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    // Every collected file, with its bucket and (for a rejected file) its
    // recovered-tree verdict token.
    let mut outcomes: Vec<(&PathBuf, Bucket, Option<String>)> = Vec::with_capacity(files.len());
    let mut range_messages: Vec<(&PathBuf, String)> = Vec::new();
    let mut recovery_divergences: Vec<(&PathBuf, String)> = Vec::new();
    for path in &files {
        let src = match read_corpus_source(path) {
            Ok(src) => src,
            Err(err) if err.is_non_utf8() => {
                outcomes.push((path, Bucket::NonUtf8, None));
                continue;
            }
            Err(err) => panic!("{err}"),
        };
        let line = fcs_ast_batch(path);
        let outcome = classify(path, &src, &line, &compiled_symbols, &script_symbols);
        if let Some(message) = outcome.range_message {
            range_messages.push((path, message));
        }
        let token = outcome.recovery.map(|r| {
            if let Some(d) = r.divergence {
                recovery_divergences.push((path, d));
            }
            r.token
        });
        outcomes.push((path, outcome.bucket, token));
    }

    let in_bucket = |b: Bucket| -> Vec<&PathBuf> {
        outcomes.iter().filter(|o| o.1 == b).map(|o| o.0).collect()
    };
    let count = |b: Bucket| outcomes.iter().filter(|o| o.1 == b).count();
    eprintln!(
        "differential: {} files | {} match | {} range-divergent | {} ast-divergent | \
         {} we-accept/fcs-reject | {} both-reject | {} we-reject/fcs-accept | \
         {} our-parse-panicked | {} our-normaliser-unmodelled | \
         {} fcs-normaliser-unmodelled | {} fcs-errors | {} fcs-record-malformed | \
         {} fcs-missing-status | {} range-audit-panicked | {} non-UTF-8",
        files.len(),
        count(Bucket::Match),
        count(Bucket::RangeDivergent),
        count(Bucket::AstDivergent),
        count(Bucket::WeAcceptFcsRejects),
        count(Bucket::BothReject),
        count(Bucket::WeRejectFcsAccepts),
        count(Bucket::OurParsePanicked),
        count(Bucket::OurNormaliserUnmodelled),
        count(Bucket::FcsNormaliserUnmodelled),
        count(Bucket::FcsError),
        count(Bucket::FcsRecordMalformed),
        count(Bucket::FcsMissingParseStatus),
        count(Bucket::RangeAuditPanicked),
        count(Bucket::NonUtf8),
    );
    for (bucket, heading) in [
        (Bucket::AstDivergent, "AST divergences"),
        (Bucket::WeAcceptFcsRejects, "We accept / FCS rejects"),
        (Bucket::NonUtf8, "Non-UTF-8 corpus sources"),
    ] {
        let paths = in_bucket(bucket);
        if !paths.is_empty() {
            eprintln!(
                "\n{heading} ({}, showing up to {DIVERGENCE_SAMPLE}):",
                paths.len()
            );
            for p in paths.iter().take(DIVERGENCE_SAMPLE) {
                eprintln!("  {}", p.display());
            }
        }
    }
    if !recovery_divergences.is_empty() {
        eprintln!(
            "\nRecovered-tree divergences ({}, showing up to {DIVERGENCE_SAMPLE}):",
            recovery_divergences.len()
        );
        for (p, d) in recovery_divergences.iter().take(DIVERGENCE_SAMPLE) {
            eprintln!("  {}\n    {}", p.display(), d.replace('\n', "\n    "));
        }
    }
    if !range_messages.is_empty() {
        eprintln!(
            "\nAST range divergences ({}, showing up to {DIVERGENCE_SAMPLE}):",
            range_messages.len()
        );
        for (p, message) in range_messages.iter().take(DIVERGENCE_SAMPLE) {
            eprintln!("  {}\n{}", p.display(), message);
        }
    }

    // The oracle-integrity gates, which no manifest regeneration may bless.
    let missing_status = in_bucket(Bucket::FcsMissingParseStatus);
    assert!(
        missing_status.is_empty(),
        "{} non-error FCS records lacked ParseHadErrors; cannot decide whether \
         recovery ASTs are acceptable: {missing_status:#?}",
        missing_status.len(),
    );
    let audit_panicked = in_bucket(Bucket::RangeAuditPanicked);
    assert!(
        audit_panicked.is_empty(),
        "{} files matched structurally but the AST range audit panicked. The \
         range oracle is not total over the structurally-matched corpus: \
         {audit_panicked:#?}",
        audit_panicked.len(),
    );

    let entries = std::iter::once(summary_entry("match", count(Bucket::Match))).chain(
        outcomes
            .iter()
            .filter(|o| o.1 != Bucket::Match)
            .map(|(p, b, token)| match token {
                Some(token) => format!("{} {} {token}", relative_key(&root, p), b.name()),
                None => format!("{} {}", relative_key(&root, p), b.name()),
            }),
    );
    let manifest =
        Manifest::from_entries(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "parser_corpus_diff",
        &manifest,
        &regenerate_ignored("parser_corpus_diff"),
    );
}

fn is_signature_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("fsi"))
}

fn is_script_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("fsx"))
}
