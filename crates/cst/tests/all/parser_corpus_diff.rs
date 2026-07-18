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
//! sides normalise. The outcomes:
//!
//! * **match** — both normalise and are equal. This is the headline coverage.
//! * **divergence** — both normalise but differ. A real signal: a parser bug,
//!   or a normaliser asymmetry. Ratcheted by [`MAX_AST_DIVERGENCES`] (drive to
//!   zero); example paths are printed so they can be investigated.
//! * **we accept / FCS rejects** — our parser is clean but FCS reports
//!   `ParseHadErrors`. Ratcheted by [`MAX_WE_ACCEPT_FCS_REJECTS`], so a
//!   recovery AST cannot be counted as a match.
//! * unmodeled — either side panicked (construct not modeled yet), both parsers
//!   rejected, or our parse had errors while FCS was clean. Expected and
//!   uninteresting; counted, not asserted.
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

use std::path::{Path, PathBuf};

use std::collections::HashSet;

use borzoi_cst::parser::{Parse, parse_sig_with_symbols, parse_with_symbols};
use serde::Deserialize;

use crate::common::catch_unwind_silent;
use crate::common::normalised_ast::{normalise_fcs_dump, normalise_parse};
use crate::common::{
    ast_ranges_match, collect_fsharp_corpus_files, corpus_root, fcs_ast_batch, read_corpus_source,
};

/// Lower bound on files where our AST equals FCS's. Only goes up — bump it
/// after a phase lands. A drop is a regression (a construct stopped matching).
///
/// Measured 2026-07-12 against the pinned F# corpus (dotnet/fsharp
/// `c3c01c99`, the compiler shipped in SDK 10.0.301): 5452 of 6344 records
/// (was 5451; the trait-call support's concrete `typarAlts` alternatives —
/// `((^T or int) : (static member …) …)` — took one more file). 2026-07-17: 5455,
/// +2 from group B (the verbose `begin … end` sig type body and the opaque-type
/// `val`-promotion — `test.fsi` and `ProvidedTypes.fsi` now match; the third is
/// pre-existing floor lag). 2026-07-17: 5457, +2 from group C (the nameless
/// `namespace` / `namespace rec` — `Namespace 05.fs` / `08.fs`). The
/// initialiser-less `member val` (`Auto property 04.fs`) was dropped as too
/// context-sensitive (see the note in `parser_diff_module_structure.rs`).
/// 2026-07-18: 5458, +1 from group D's parenthesised-pattern member head
/// (`static member (y) …` — `neg133.fs`). The other group-D file — the two-token
/// range-step operator name `(.. ..)` = `op_RangeStep` (FCS's `operatorName:
/// DOT_DOT DOT_DOT`, `productioncoverage01.fs`) — was implemented then **dropped**
/// as a deliberate doom-loop exit, and stays a documented `we_reject_fcs_accepts`
/// divergence. The parser can *accept* it (the differential matched at 5459), but
/// it is the one paren operator name spanning two lexer tokens, so a faithful
/// representation is a wrapper *node* (`RANGE_STEP_OP`, like the active-pattern
/// name) — and, exactly like an active-pattern name, that node is invisible to
/// `LongIdent::idents()`, so **every** `idents()` consumer must add a parallel
/// `range_step_op()` guard or silently drop the segment: ~15–20 sites across
/// `infer.rs` / `resolve/types.rs` (else a path like `"hi".(.. ..).Length`
/// mis-infers to `"hi".Length`), plus a leading-trivia-excluding node range and a
/// multi-segment accessor for `(.. ..).(.. ..)`. (Three review rounds each
/// surfaced a deeper layer.) That is active-pattern-scale plumbing across the whole
/// semantic engine for an operator no real code redefines — not worth it.
const MIN_AST_MATCHES: usize = 5458;

/// Upper bound on files that normalise on both sides but disagree. Drive this
/// to zero: each one is a parser bug or a normaliser asymmetry. (The
/// implicit-`COMPILED`/`EDITING` class is gone now that both sides define
/// them.) The divergent paths are printed so they can be triaged.
///
/// Measured 2026-07-09 against the pinned F# corpus (dotnet/fsharp
/// `c3c01c99`).
const MAX_AST_DIVERGENCES: usize = 1;

/// Lower bound on files where our AST equals FCS's and the audited broad AST
/// ranges (modules and declarations) also equal FCS's. Only goes up as range
/// handling is tightened.
///
/// Measured 2026-07-12 against the pinned F# corpus (dotnet/fsharp
/// `c3c01c99`): 5424, up from 5423 with the newly-matching concrete
/// trait-call support alternative. (The earlier 5427 was against the old
/// `bdb847ab` corpus — that tree changed, not our range handling.) 2026-07-17:
/// 5427, the two group-B files' ranges also match (plus pre-existing floor lag).
/// (Group C's two nameless-namespace files match shape but their broad ranges
/// diverge — see [`MAX_AST_RANGE_DIVERGENCES`] — so they do not lift this floor.)
/// 2026-07-18: 5428, group D's `neg133.fs` matches shape and broad ranges (the
/// range-step file was dropped — see [`MIN_AST_MATCHES`]).
const MIN_AST_RANGE_MATCHES: usize = 5428;

/// Upper bound on files whose normalised AST shape matches FCS but whose
/// audited broad AST ranges diverge. This starts high because the first generic
/// rule intentionally compares only trimmed CST node spans; FCS often includes
/// XML docs/attributes or a final module newline in declaration/module ranges.
/// Drive this down as those cases become explicit.
///
/// Measured 2026-07-09 against the pinned F# corpus (dotnet/fsharp
/// `c3c01c99`): exactly 28, unchanged from the old corpus. 2026-07-17: 30 — the
/// two nameless-namespace files group C now accepts (`Namespace 05.fs` / `08.fs`)
/// match shape but their broad module range diverges from FCS (an empty-`longId`
/// `DeclaredNamespace`'s span; the same approximate broad-range class this ceiling
/// already tolerates), so they land here rather than in the range-match count.
const MAX_AST_RANGE_DIVERGENCES: usize = 30;

/// Upper bound on files where our parser is clean but FCS reports
/// `ParseHadErrors`. These are parser acceptance gaps, but a known corpus
/// bucket for now (typically negative `E_*` fixtures). Keep this ratcheted so a
/// recoverable FCS AST cannot silently inflate [`MIN_AST_MATCHES`].
///
/// Measured 2026-07-10 against the pinned F# corpus (dotnet/fsharp
/// `c3c01c99`) with the general offside FS0058 emission
/// (`docs/offside-diagnostics-plan.md`, §A) in place: emitting the offside
/// diagnostic moves the affected files from "we accept" to "both reject",
/// leaving 29. The margin absorbs the FCS `ast-batch` accept/reject
/// nondeterminism seen on a few blank-line-sensitive fixtures.
const MAX_WE_ACCEPT_FCS_REJECTS: usize = 31;

/// Upper bound on real corpus files that are not UTF-8 source. These are
/// explicit skips because our parser takes `&str`; I/O failures still panic.
///
/// Measured 2026-07-09 against the pinned F# corpus (dotnet/fsharp
/// `c3c01c99`): 11 codepage / UTF-16 fixtures.
const MAX_NON_UTF8_SOURCES: usize = 11;

/// How many divergent paths to print for investigation.
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

#[derive(Default)]
struct Tally {
    seen: usize,
    matches: usize,
    /// Files whose normalised AST shape matches and whose audited ranges match.
    range_matches: usize,
    divergences: Vec<PathBuf>,
    /// Shape matches, but the separately-audited source ranges diverge.
    range_divergences: Vec<(PathBuf, String)>,
    /// The range audit itself panicked after shape equality succeeded.
    range_skipped: usize,
    /// `ast-batch` reported a per-file FCS failure (`{Path, Error}`).
    fcs_errors: usize,
    /// FCS side didn't yield a model: either `from_fcs` doesn't model the
    /// construct yet, or the AST nests beyond `serde_json`'s default recursion
    /// limit (a pathologically deep file). Skipped — and, by running before our
    /// parse, this keeps our recursive-descent parser off such deep input.
    fcs_skipped: usize,
    /// Our parser produced errors, so its tree isn't meaningful to diff.
    /// This is counted only when FCS accepted the file.
    our_errors: usize,
    /// Both parsers rejected the file; no AST equality signal to gate.
    both_parse_errors: usize,
    /// Our parser accepted a file FCS rejected. Ratcheted separately from AST
    /// divergence because FCS may still emit a recovery tree.
    we_accept_fcs_rejects: Vec<PathBuf>,
    /// Our parse or our normalisation panicked (construct not modeled yet).
    our_skipped: usize,
    /// `BatchMeta` itself didn't parse (malformed JSONL line — not expected).
    json_skipped: usize,
    /// A non-error `ast-batch` record lacked `ParseHadErrors`.
    fcs_missing_parse_status: usize,
    /// Source files that are not UTF-8 and therefore cannot be fed to our
    /// `&str` parser. Ratcheted separately from unreadable files, which panic.
    non_utf8: Vec<PathBuf>,
}

fn parse_ours(
    path: &Path,
    src: &str,
    symbols: &HashSet<String>,
    tally: &mut Tally,
) -> Option<Parse> {
    let is_sig = is_signature_path(path);
    // The raw parser panics on a few constructs (tracked by the
    // `parser_corpus.rs` panic ceiling); catch so one file can't abort the
    // whole sweep.
    match catch_unwind_silent(|| {
        if is_sig {
            parse_sig_with_symbols(src, symbols)
        } else {
            parse_with_symbols(src, symbols)
        }
    }) {
        Ok(ours) => Some(ours),
        Err(_) => {
            tally.our_skipped += 1;
            None
        }
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

    let mut tally = Tally::default();

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

    for expected_path in &files {
        let src = match read_corpus_source(expected_path) {
            Ok(src) => src,
            Err(err) if err.is_non_utf8() => {
                tally.non_utf8.push(expected_path.clone());
                continue;
            }
            Err(err) => panic!("{err}"),
        };
        let line = fcs_ast_batch(expected_path);
        tally.seen += 1;
        // `BatchMeta` ignores the heavy `ParseTree` field, so this stays cheap
        // and — unlike a full `Value` parse — does not trip the recursion limit
        // on deep files.
        let Ok(meta) = serde_json::from_str::<BatchMeta>(&line) else {
            tally.json_skipped += 1;
            continue;
        };
        let path = PathBuf::from(&meta.path);
        assert_eq!(
            path.as_path(),
            expected_path.as_path(),
            "fcs-dump ast-batch response path did not match request"
        );

        if meta.error.is_some() {
            tally.fcs_errors += 1;
            continue;
        }

        assert_eq!(
            meta.is_script,
            Some(is_script_path(&path)),
            "fcs-dump ast-batch script classification did not match request"
        );

        let Some(fcs_had_errors) = meta.parse_had_errors else {
            tally.fcs_missing_parse_status += 1;
            continue;
        };

        if fcs_had_errors {
            let symbols = if is_script_path(&path) {
                &script_symbols
            } else {
                &compiled_symbols
            };
            let Some(ours) = parse_ours(&path, &src, symbols, &mut tally) else {
                continue;
            };
            if ours.errors.is_empty() {
                tally.we_accept_fcs_rejects.push(path);
            } else {
                tally.both_parse_errors += 1;
            }
            continue;
        }

        // Normalise the FCS side *first*. Its internal full `Value` parse fails
        // (caught here) on trees deeper than `serde_json`'s default recursion
        // limit, so a pathologically deep file is skipped before our own
        // recursive-descent parser ever runs on it — keeping that parser, which
        // we do not run under a guard stack, off stack-overflow-deep input.
        let Ok(fcs_norm) = catch_unwind_silent(|| normalise_fcs_dump(&line)) else {
            tally.fcs_skipped += 1;
            continue;
        };

        // Our side: parse the same source. We only compare where *we* are
        // clean and FCS is clean — an error tree is not a meaningful thing to
        // diff.
        let symbols = if is_script_path(&path) {
            &script_symbols
        } else {
            &compiled_symbols
        };
        let Some(ours) = parse_ours(&path, &src, symbols, &mut tally) else {
            continue;
        };
        if !ours.errors.is_empty() {
            tally.our_errors += 1;
            continue;
        }

        let Ok(ours_norm) = catch_unwind_silent(|| normalise_parse(&ours)) else {
            tally.our_skipped += 1;
            continue;
        };

        if ours_norm == fcs_norm {
            tally.matches += 1;
            match catch_unwind_silent(|| ast_ranges_match(&ours, &line, &src)) {
                Ok(Ok(())) => tally.range_matches += 1,
                Ok(Err(message)) => tally.range_divergences.push((path, message)),
                Err(_) => tally.range_skipped += 1,
            }
        } else {
            tally.divergences.push(path);
        }
    }

    eprintln!(
        "differential: {} records | {} shape-match | {} range-match | \
         {} shape-diverge | {} range-diverge | {} range-skipped | \
         {} our-errors/fcs-clean | {} we-accept/fcs-reject | {} both-errors | \
         {} our-skipped | {} fcs-skipped | {} fcs-errors | \
         {} fcs-missing-status | {} json-skipped | {} non-UTF-8 skipped",
        tally.seen,
        tally.matches,
        tally.range_matches,
        tally.divergences.len(),
        tally.range_divergences.len(),
        tally.range_skipped,
        tally.our_errors,
        tally.we_accept_fcs_rejects.len(),
        tally.both_parse_errors,
        tally.our_skipped,
        tally.fcs_skipped,
        tally.fcs_errors,
        tally.fcs_missing_parse_status,
        tally.json_skipped,
        tally.non_utf8.len(),
    );

    assert_eq!(
        tally.seen + tally.non_utf8.len(),
        files.len(),
        "fcs-dump ast-batch produced {} JSONL records, skipped {} non-UTF-8 \
         files, but {} paths were collected",
        tally.seen,
        tally.non_utf8.len(),
        files.len(),
    );

    if !tally.divergences.is_empty() {
        eprintln!(
            "\nAST divergences ({}, showing up to {}):",
            tally.divergences.len(),
            DIVERGENCE_SAMPLE
        );
        for p in tally.divergences.iter().take(DIVERGENCE_SAMPLE) {
            eprintln!("  {}", p.display());
        }
    }

    if !tally.range_divergences.is_empty() {
        eprintln!(
            "\nAST range divergences ({}, showing up to {}):",
            tally.range_divergences.len(),
            DIVERGENCE_SAMPLE
        );
        for (p, message) in tally.range_divergences.iter().take(DIVERGENCE_SAMPLE) {
            eprintln!("  {}\n{}", p.display(), message);
        }
    }

    if !tally.we_accept_fcs_rejects.is_empty() {
        eprintln!(
            "\nWe accept / FCS rejects ({}, showing up to {}):",
            tally.we_accept_fcs_rejects.len(),
            DIVERGENCE_SAMPLE
        );
        for p in tally.we_accept_fcs_rejects.iter().take(DIVERGENCE_SAMPLE) {
            eprintln!("  {}", p.display());
        }
    }
    if !tally.non_utf8.is_empty() {
        eprintln!("\nNon-UTF-8 corpus sources ({}):", tally.non_utf8.len());
        for p in &tally.non_utf8 {
            eprintln!("  {}", p.display());
        }
    }

    assert_eq!(
        tally.fcs_missing_parse_status, 0,
        "{} non-error FCS records lacked ParseHadErrors; cannot decide whether \
         recovery ASTs are acceptable.",
        tally.fcs_missing_parse_status,
    );
    assert!(
        tally.non_utf8.len() <= MAX_NON_UTF8_SOURCES,
        "{} corpus sources were not UTF-8 (ceiling is MAX_NON_UTF8_SOURCES = {}). \
         These are skipped explicitly because the CST parser takes &str; \
         investigate new entries rather than silently dropping them.",
        tally.non_utf8.len(),
        MAX_NON_UTF8_SOURCES,
    );
    assert!(
        tally.we_accept_fcs_rejects.len() <= MAX_WE_ACCEPT_FCS_REJECTS,
        "{} files parse cleanly on our side but FCS reports ParseHadErrors \
         (ceiling is MAX_WE_ACCEPT_FCS_REJECTS = {}). A parser acceptance gap \
         regressed in.",
        tally.we_accept_fcs_rejects.len(),
        MAX_WE_ACCEPT_FCS_REJECTS,
    );
    assert!(
        tally.divergences.len() <= MAX_AST_DIVERGENCES,
        "{} files normalise on both sides but disagree (ceiling is \
         MAX_AST_DIVERGENCES = {}). A parser bug or normaliser asymmetry \
         regressed in.",
        tally.divergences.len(),
        MAX_AST_DIVERGENCES,
    );
    assert_eq!(
        tally.range_skipped, 0,
        "{} files matched structurally but the AST range audit panicked. The \
         range oracle is not total over the structurally-matched corpus.",
        tally.range_skipped,
    );
    assert!(
        tally.range_divergences.len() <= MAX_AST_RANGE_DIVERGENCES,
        "{} files match structurally but have audited AST range divergences \
         (ceiling is MAX_AST_RANGE_DIVERGENCES = {}). The broad range-audit \
         coverage regressed.",
        tally.range_divergences.len(),
        MAX_AST_RANGE_DIVERGENCES,
    );
    assert!(
        tally.range_matches >= MIN_AST_RANGE_MATCHES,
        "only {} files match FCS ranges after structural AST equality \
         (floor is MIN_AST_RANGE_MATCHES = {}). Audited range coverage \
         regressed.",
        tally.range_matches,
        MIN_AST_RANGE_MATCHES,
    );
    assert!(
        tally.matches >= MIN_AST_MATCHES,
        "only {} files match FCS (floor is MIN_AST_MATCHES = {}). AST matches \
         regressed.",
        tally.matches,
        MIN_AST_MATCHES,
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
