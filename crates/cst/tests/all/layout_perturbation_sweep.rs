//! Panic-freedom under layout damage: break a corpus file's line before one
//! of its tokens and reparse.
//!
//! `bump_into` refuses to stamp a real token's kind onto a layout virtual
//! (debug builds panic), and the class of production that does so is one that
//! approves a token through a *raw*-stream lookahead while the filtered cursor
//! is parked on a virtual LexFilter inserted in front of it. Layout virtuals
//! come from line breaks: a token pushed to column 0 is offside of nearly
//! every context, so LexFilter closes blocks or separates statements right
//! before it. The token-soup sweeps (`parser_panic_sweep`) reach that state
//! only by chance; real files put every production in front of it.
//!
//! So: for every corpus file, at [`POSITIONS_PER_FILE`] token starts drawn
//! evenly through it, break the line there — the token landing at column 0,
//! column 1, or one column past its own line's indentation ([`line_breaks`]:
//! offside of nearly everything, of all but the outermost context, or of only
//! the innermost ones) — and parse at the oldest and the newest
//! language versions (the offside rules differ across them). No parse may
//! panic or drop bytes. Failures are bucketed by panic message, each with the
//! first witness: the file, the byte offset of the inserted newline, and the
//! version.
//!
//! `#[ignore]`d: it walks the pinned corpus (`BORZOI_CORPUS`, set under
//! `nix develop`), about ten minutes in a debug build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::lexer::lex;
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};
use borzoi_oracle_harness::panic_silence::take_silenced_panic;

use crate::common::{
    catch_unwind_silent, collect_fsharp_corpus_files, corpus_root, read_corpus_source,
};

/// Newline insertion points per file, spread evenly over its tokens.
const POSITIONS_PER_FILE: usize = 8;

/// The offside rules change at F# 6 and again at F# 8, so the two ends of the
/// range take different paths through LexFilter.
const VERSIONS: [LanguageVersion; 2] = [LanguageVersion::MIN, LanguageVersion::MAX];

/// Byte offsets of up to [`POSITIONS_PER_FILE`] token starts, evenly spaced,
/// skipping tokens that already begin a line.
fn insertion_points(src: &str) -> Vec<usize> {
    let starts: Vec<usize> = lex(src)
        .filter(|(tok, _)| tok.is_ok())
        .map(|(_, span)| span.start)
        .filter(|&start| start > 0 && src.as_bytes()[start - 1] != b'\n')
        .collect();
    if starts.is_empty() {
        return Vec::new();
    }
    let n = POSITIONS_PER_FILE.min(starts.len());
    (0..n).map(|i| starts[i * starts.len() / n]).collect()
}

/// The first witness of one failure cause.
struct Witness {
    file: PathBuf,
    offset: usize,
    line_break: String,
    lang: LanguageVersion,
    count: usize,
}

/// Parse `src` with each line break at each insertion point, recording each
/// failure under its cause.
fn sweep_file(file: &Path, src: &str, failures: &Mutex<BTreeMap<String, Witness>>) {
    let kind = if file.extension().is_some_and(|e| e == "fsi") {
        FileKind::Sig
    } else {
        FileKind::Impl
    };
    for offset in insertion_points(src) {
        for line_break in line_breaks(src, offset) {
            let damaged = format!("{}{line_break}{}", &src[..offset], &src[offset..]);
            sweep_damaged(file, kind, &damaged, offset, &line_break, failures);
        }
    }
}

/// The line breaks inserted before the token at `offset`: a newline then the
/// token at column 0, at column 1, and one column past the indentation of the
/// line it was on (when that is another column again).
fn line_breaks(src: &str, offset: usize) -> Vec<String> {
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let indent = src[line_start..].bytes().take_while(|&b| b == b' ').count();
    let mut breaks = vec!["\n".to_string(), "\n ".to_string()];
    if indent > 0 {
        breaks.push(format!("\n{}", " ".repeat(indent + 1)));
    }
    breaks
}

/// Parse one damaged source at each version, recording each failure under its
/// cause.
fn sweep_damaged(
    file: &Path,
    kind: FileKind,
    damaged: &str,
    offset: usize,
    line_break: &str,
    failures: &Mutex<BTreeMap<String, Witness>>,
) {
    let symbols = std::collections::HashSet::new();
    for lang in VERSIONS {
        let parsed = catch_unwind_silent(|| {
            parse_with_options(
                damaged,
                ParseOptions {
                    file_kind: kind,
                    symbols: &symbols,
                    lang,
                },
            )
        });
        let cause = match parsed {
            Ok(parse) if parse.root.text() == damaged => continue,
            Ok(_) => "lossless round-trip violated".to_string(),
            Err(_) => {
                let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
                // The stamp check appends the span; the cause is the rest.
                let message = message.split(" at ").next().unwrap_or_default();
                format!("panic: {message}")
            }
        };
        let mut failures = failures.lock().expect("no poisoning: panics are caught");
        failures
            .entry(cause)
            .or_insert_with(|| Witness {
                file: file.to_path_buf(),
                offset,
                line_break: line_break.to_string(),
                lang,
                count: 0,
            })
            .count += 1;
    }
}

#[test]
#[ignore = "walks the pinned corpus; about ten minutes"]
fn a_line_break_before_any_token_parses_without_panicking() {
    let root = corpus_root();
    let files = collect_fsharp_corpus_files(&root).expect("walk the corpus");
    let failures = Mutex::new(BTreeMap::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(file) = files.get(i) else { break };
                    let Ok(src) = read_corpus_source(file) else {
                        continue;
                    };
                    sweep_file(file, &src, &failures);
                }
            });
        }
    });
    let failures = failures.into_inner().expect("no poisoning");
    let report: Vec<String> = failures
        .iter()
        .map(|(cause, w)| {
            format!(
                "{cause} ({} cases)\n    first seen: {} with {:?} inserted at byte {}, {:?}",
                w.count,
                w.file.display(),
                w.line_break,
                w.offset,
                w.lang
            )
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} distinct failure cause(s) over {} files:\n{}",
        failures.len(),
        files.len(),
        report.join("\n")
    );
}
