//! Parse-only throughput benchmark for the `borzoi-cst` parser.
//!
//! Reads newline-separated file paths from stdin, loads every strictly-UTF-8
//! file into memory ONCE, then parses the whole set `iterations` times
//! (argv[1], default 1), printing per-iteration wall time to stderr and a
//! checksum + file count to stdout.
//!
//! Only the parse loop is timed — file IO and per-file classification happen
//! once up front, outside the timed region — so this measures the parser, not
//! the harness. Run it under `time(1)` with `iterations = 1` for a cold-start
//! (process launch → all files parsed) number; run with several iterations and
//! read the later ones for warm steady-state throughput (a Rust AOT binary has
//! no JIT to warm, but the allocator, CPU caches and OS page cache still settle
//! after iteration 1).
//!
//! To be a like-for-like comparison against `fcs-dump parse-bench`, the two
//! sides must parse the *same* code:
//!   * **Implicit symbols.** `FSharpChecker.ParseFile` always defines
//!     `COMPILED`+`EDITING` for a compiled `.fs`/`.fsi` and `INTERACTIVE`+
//!     `EDITING` for a `.fsx` script, even with empty `ConditionalDefines`.
//!     We pass the matching set so `#if COMPILED` etc. select the same branch.
//!   * **Language version.** We parse at [`LanguageVersion::DEFAULT`] (F# 10.0),
//!     which equals FCS's own default when `BORZOI_FCS_LANGVERSION` is
//!     unset — so no environment configuration is needed to keep the version-
//!     gated behaviour (strict indentation, `#elif`) aligned.
//!   * **Encoding.** Both sides decode strict UTF-8 and skip anything else
//!     (`String::from_utf8` here; a throwing `UTF8Encoding` on the FCS side),
//!     so the parsed file set is identical rather than differing by the corpus's
//!     handful of UTF-16 / code-page files. Both then strip a leading UTF-8 BOM,
//!     so the source is byte-identical and FCS is not fed BOMs its real consumers
//!     would have stripped.
//!
//! Build optimised, feed both sides the same path list:
//!   cargo build -p borzoi-cst --release --example parse_bench
//!   target/release/examples/parse_bench 6 < paths.txt

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, ParseOptions, parse_with_options};

fn main() {
    // Some real corpus files still panic our parser (see the `fcs_divergence`
    // "parser panics" bucket). Each parse runs under `catch_unwind` so one
    // panicking file is counted, not fatal; silence the hook so the panic
    // messages don't drown the timing output. This binary is its own process,
    // so swapping the global hook here is safe.
    std::panic::set_hook(Box::new(|_| {}));

    let iterations: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    // FCS's service-parser implicit symbol sets, matched exactly (see module
    // docs): compiled files get COMPILED+EDITING, scripts INTERACTIVE+EDITING.
    let compiled_symbols: HashSet<String> = ["COMPILED", "EDITING"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let script_symbols: HashSet<String> = ["INTERACTIVE", "EDITING"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("read stdin");

    // Load + classify every file once. `kind`/`is_script` drive the top-level
    // production (`.fsi` → signature file) and the implicit-symbol set.
    let mut sources: Vec<String> = Vec::new();
    let mut kinds: Vec<(FileKind, bool)> = Vec::new();
    let mut non_utf8 = 0usize;
    let mut io_err = 0usize;
    for line in input.lines() {
        let path = line.trim();
        if path.is_empty() {
            continue;
        }
        match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(mut text) => {
                    // Strip a leading UTF-8 BOM so both sides parse byte-identical,
                    // BOM-free source. FCS's real consumers strip it; our lexer
                    // treats U+FEFF as whitespace, so removing it changes neither
                    // the resulting tree nor the timing.
                    if text.starts_with('\u{feff}') {
                        text.drain(..'\u{feff}'.len_utf8());
                    }
                    let ext = Path::new(path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(str::to_ascii_lowercase);
                    // Mirror fcs-dump's `isScriptPath` / FCS's
                    // `FSharpScriptFileSuffixes` exactly: both `.fsx` and
                    // `.fsscript` are impl-kind scripts (INTERACTIVE+EDITING);
                    // `.fsi` is a signature file; everything else is a compiled
                    // impl. A mismatch here would desync `#if INTERACTIVE` /
                    // `#if COMPILED` branch selection between the two sides.
                    let kind = match ext.as_deref() {
                        Some("fsi") => (FileKind::Sig, false),
                        Some("fsx" | "fsscript") => (FileKind::Impl, true),
                        _ => (FileKind::Impl, false),
                    };
                    sources.push(text);
                    kinds.push(kind);
                }
                Err(_) => non_utf8 += 1,
            },
            Err(_) => io_err += 1,
        }
    }

    let n = sources.len();
    let total_bytes: usize = sources.iter().map(String::len).sum();
    eprintln!(
        "rust: loaded {n} files ({total_bytes} bytes); skipped {non_utf8} non-utf8, {io_err} io-err"
    );

    let mut checksum: u64 = 0;
    let mut panics = 0usize;
    for it in 1..=iterations {
        let start = Instant::now();
        let mut local_checksum: u64 = 0;
        let mut local_panics = 0usize;
        for (src, &(file_kind, is_script)) in sources.iter().zip(&kinds) {
            let src = src.as_str();
            let symbols = if is_script {
                &script_symbols
            } else {
                &compiled_symbols
            };
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let p = parse_with_options(
                    src,
                    ParseOptions {
                        file_kind,
                        symbols,
                        lang: LanguageVersion::DEFAULT,
                    },
                );
                u64::from(u32::from(p.root.text_range().len())) + p.errors.len() as u64
            }));
            match res {
                Ok(c) => local_checksum = local_checksum.wrapping_add(c),
                Err(_) => local_panics += 1,
            }
        }
        let elapsed = start.elapsed();
        let secs = elapsed.as_secs_f64();
        checksum = local_checksum;
        panics = local_panics;
        eprintln!(
            "rust: iter {it}/{iterations}  {:.3} ms  ({:.0} files/s, {:.1} MB/s)  panics={local_panics}",
            secs * 1e3,
            n as f64 / secs,
            (total_bytes as f64 / 1e6) / secs,
        );
    }
    println!("rust checksum={checksum} files={n} panics={panics}");
}
