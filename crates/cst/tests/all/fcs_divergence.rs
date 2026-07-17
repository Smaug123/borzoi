//! Regenerate the categorised parser-vs-FCS divergence report.
//!
//! `parser_corpus_diff.rs` *asserts* a floor of AST matches and a ceiling of
//! divergences over the corpus, but it only ever prints a sample of paths. When
//! you actually want to *triage* — "which files do we reject that FCS accepts,
//! and with what error?" — you need the full categorised lists. This test is
//! the generator for those: it sweeps the same corpus, classifies every
//! `.fs`/`.fsi` into one bucket, and writes one file per bucket under
//! `fcs-divergence/` (override with `BORZOI_DIVERGENCE_OUT`).
//!
//! The buckets, keyed on whether each side's *parse* succeeded (FCS:
//! `ParseHadErrors == false`; ours: `errors.is_empty()`), then on AST equality
//! when both are clean:
//!
//! * `both_reject.txt` — both parsers report errors. One path per line.
//! * `we_reject_fcs_accepts.txt` — we error, FCS is clean. `<our first error
//!   message>\t<path>`, sorted by message then path, so the dominant gaps group
//!   together. This is the primary worklist.
//! * `we_accept_fcs_rejects.txt` — we are clean, FCS errors (typically negative
//!   `E_*` test fixtures we don't yet reject). One path per line.
//! * `ast_divergence.txt` — both clean and both normalise, but the normalised
//!   ASTs differ. A real parser bug or a normaliser asymmetry. One path per line.
//! * `uncompared_fcs_unmodeled.txt` — both clean, but normalising one side
//!   panicked: a construct the shared normaliser doesn't model yet (or an AST
//!   too deep for `serde_json`). Can't be compared. One path per line.
//! * `our_parser_panics.txt` — our parser itself panicked. One path per line.
//! * `uncompared_other.txt` — couldn't even get that far: `(json parse failure)`
//!   (the JSONL line didn't parse), `<path>\t(unreadable)` (we couldn't read the
//!   source), or `<path>\t(fcs error: …)` (a per-file FCS failure in the batch).
//!
//! Files that match (both clean, both normalise, equal ASTs) are the headline
//! success case and are only counted, not listed.
//!
//! Like the other corpus sweeps this is `#[ignore]`d (it parses the corpus
//! twice and writes report files). FCS is driven through the shared
//! request/response `ast-batch` wrapper, so each file has a bounded timeout and
//! the oracle child is respawned on a wedge/crash. Run with
//! `cargo test -p borzoi-cst --test all fcs_divergence:: -- --ignored --nocapture`
//! under `nix develop` (which sets `BORZOI_CORPUS`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use borzoi_cst::parser::{parse_sig_with_symbols, parse_with_symbols};
use serde::Deserialize;

use crate::common::normalised_ast::{normalise_fcs_dump, normalise_parse};
use crate::common::{catch_unwind_silent, corpus_root, fcs_ast_batch};

/// Lightweight view of one `ast-batch` JSONL record. Ignores the heavy
/// `ParseTree` so this stays cheap *and* — unlike a full `Value` parse — does
/// not trip `serde_json`'s recursion limit on deeply-nested files. Carries
/// `ParseHadErrors` (the FCS accept/reject signal) and `Error` (a per-file
/// batch failure: `{Path, Error}` with no parse fields).
#[derive(Deserialize)]
struct BatchMeta {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Error")]
    error: Option<String>,
    #[serde(rename = "ParseHadErrors")]
    parse_had_errors: Option<bool>,
}

#[derive(Default)]
struct Buckets {
    /// `(message, path)` — sorted by message then path on write.
    we_reject_fcs_accepts: Vec<(String, PathBuf)>,
    we_accept_fcs_rejects: Vec<PathBuf>,
    both_reject: Vec<PathBuf>,
    ast_divergence: Vec<PathBuf>,
    uncompared_fcs_unmodeled: Vec<PathBuf>,
    our_parser_panics: Vec<PathBuf>,
    /// Free-form annotated lines (already include their own annotation).
    uncompared_other: Vec<String>,
    /// Both clean, both normalise, equal — counted only.
    matches: usize,
    seen: usize,
}

#[test]
#[ignore = "full-corpus categorised divergence report (us + FCS); run with --ignored under nix develop"]
fn regenerate_fcs_divergence() {
    let root = corpus_root();
    let out_dir = output_dir();

    let mut files = Vec::new();
    collect_diff_files(&root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "no .fs/.fsi files under {root:?}");

    eprintln!(
        "categorising {} files under {} -> {}",
        files.len(),
        root.display(),
        out_dir.display(),
    );

    // Same implicit symbol set FCS's service parser defines for a compiled
    // `.fs`/`.fsi`, so `#if COMPILED` / `#if EDITING` branches agree instead of
    // diverging on symbol-set mismatch.
    let symbols: HashSet<String> = ["COMPILED", "EDITING"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut b = Buckets::default();

    for path in &files {
        let line = fcs_ast_batch(path);
        b.seen += 1;
        classify(&line, path, &symbols, &mut b);
    }

    assert_eq!(
        b.seen,
        files.len(),
        "fcs-dump ast-batch produced {} JSONL records but {} paths were sent",
        b.seen,
        files.len(),
    );

    write_report(&out_dir, &b);
}

/// Classify one `ast-batch` JSONL record into a bucket.
fn classify(line: &str, expected_path: &Path, symbols: &HashSet<String>, b: &mut Buckets) {
    // `BatchMeta` ignores the heavy `ParseTree`, so this stays cheap and does
    // not trip the recursion limit on deep files.
    let Ok(meta) = serde_json::from_str::<BatchMeta>(line) else {
        b.uncompared_other.push("(json parse failure)".to_string());
        return;
    };
    let path = PathBuf::from(&meta.path);
    assert_eq!(
        path.as_path(),
        expected_path,
        "fcs-dump ast-batch response path did not match request"
    );

    if let Some(err) = &meta.error {
        // Per-file FCS failure in the batch (`{Path, Error}`): nothing to
        // compare against.
        b.uncompared_other.push(format!(
            "{}\t(fcs error: {})",
            path.display(),
            sanitise(err)
        ));
        return;
    }

    let Some(fcs_had_errors) = meta.parse_had_errors else {
        b.uncompared_other
            .push(format!("{}\t(no ParseHadErrors field)", path.display()));
        return;
    };

    // Our side: parse the same source. Our recursive-descent parser has its own
    // depth limit (it errors rather than overflows on deep input), so it is safe
    // to run on every file; still, guard against any genuine panic.
    let Ok(src) = std::fs::read_to_string(&path) else {
        b.uncompared_other
            .push(format!("{}\t(unreadable)", path.display()));
        return;
    };
    let is_sig = path.extension().and_then(|s| s.to_str()) == Some("fsi");
    let Ok(ours) = catch_unwind_silent(|| {
        if is_sig {
            parse_sig_with_symbols(&src, symbols)
        } else {
            parse_with_symbols(&src, symbols)
        }
    }) else {
        b.our_parser_panics.push(path);
        return;
    };
    let our_had_errors = !ours.errors.is_empty();

    match (our_had_errors, fcs_had_errors) {
        (true, true) => b.both_reject.push(path),
        (true, false) => {
            let reason = ours
                .errors
                .first()
                .map(|e| sanitise(&e.message))
                .unwrap_or_else(|| "(no message)".to_string());
            b.we_reject_fcs_accepts.push((reason, path));
        }
        (false, true) => b.we_accept_fcs_rejects.push(path),
        (false, false) => {
            // Both accept: compare normalised ASTs. Either side's normaliser
            // panics on a construct it doesn't model yet — caught here, the file
            // is recorded as uncompared rather than aborting the sweep.
            let Ok(fcs_norm) = catch_unwind_silent(|| normalise_fcs_dump(line)) else {
                b.uncompared_fcs_unmodeled.push(path);
                return;
            };
            let Ok(ours_norm) = catch_unwind_silent(|| normalise_parse(&ours)) else {
                b.uncompared_fcs_unmodeled.push(path);
                return;
            };
            if ours_norm == fcs_norm {
                b.matches += 1;
            } else {
                b.ast_divergence.push(path);
            }
        }
    }
}

/// Collapse whitespace (tabs/newlines) in a message so a `\t`-separated report
/// line stays one line and one column.
fn sanitise(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn write_report(out_dir: &Path, b: &Buckets) {
    std::fs::create_dir_all(out_dir)
        .unwrap_or_else(|e| panic!("create {}: {e}", out_dir.display()));

    let mut both_reject = b.both_reject.clone();
    both_reject.sort();
    write_paths(out_dir, "both_reject.txt", &both_reject);

    let mut we_accept = b.we_accept_fcs_rejects.clone();
    we_accept.sort();
    write_paths(out_dir, "we_accept_fcs_rejects.txt", &we_accept);

    let mut ast_div = b.ast_divergence.clone();
    ast_div.sort();
    write_paths(out_dir, "ast_divergence.txt", &ast_div);

    let mut fcs_unmodeled = b.uncompared_fcs_unmodeled.clone();
    fcs_unmodeled.sort();
    write_paths(out_dir, "uncompared_fcs_unmodeled.txt", &fcs_unmodeled);

    let mut panics = b.our_parser_panics.clone();
    panics.sort();
    write_paths(out_dir, "our_parser_panics.txt", &panics);

    // Sorted by message then path, so the dominant gaps group together.
    let mut we_reject = b.we_reject_fcs_accepts.clone();
    we_reject.sort();
    let we_reject_lines: Vec<String> = we_reject
        .iter()
        .map(|(msg, p)| format!("{msg}\t{}", p.display()))
        .collect();
    write_lines(out_dir, "we_reject_fcs_accepts.txt", &we_reject_lines);

    let mut other = b.uncompared_other.clone();
    other.sort();
    write_lines(out_dir, "uncompared_other.txt", &other);

    eprintln!(
        "\n{} records | {} match | {} we-reject/fcs-accept | {} we-accept/fcs-reject | \
         {} both-reject | {} ast-divergence | {} fcs-unmodeled | {} our-panics | {} other",
        b.seen,
        b.matches,
        b.we_reject_fcs_accepts.len(),
        b.we_accept_fcs_rejects.len(),
        b.both_reject.len(),
        b.ast_divergence.len(),
        b.uncompared_fcs_unmodeled.len(),
        b.our_parser_panics.len(),
        b.uncompared_other.len(),
    );
    eprintln!("wrote report to {}", out_dir.display());
}

fn write_paths(out_dir: &Path, name: &str, paths: &[PathBuf]) {
    let lines: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    write_lines(out_dir, name, &lines);
}

fn write_lines(out_dir: &Path, name: &str, lines: &[String]) {
    let path = out_dir.join(name);
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    std::fs::write(&path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Where the report is written. Defaults to `fcs-divergence/` at the workspace
/// root; override with `BORZOI_DIVERGENCE_OUT`.
fn output_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BORZOI_DIVERGENCE_OUT") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("fcs-divergence")
}

/// Collect `.fs` / `.fsi` files. `.fsx` is excluded: our parser has no
/// script-specific mode, so FCS's script parse would not be a like-for-like
/// comparison. (Mirrors `parser_corpus_diff.rs`.)
fn collect_diff_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
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
            collect_diff_files(&path, out);
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("fs" | "fsi")
        ) {
            out.push(path);
        }
    }
}
