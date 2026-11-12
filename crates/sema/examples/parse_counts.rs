//! Deterministic per-phase counts for the parser's token pipeline over a
//! corpus: lexing, the lex-filter, and the whole parse, each reported as
//! retired instructions, heap allocations and bytes allocated, all per source
//! byte, beside the wall time. `docs/token-pipeline-soa-plan.md` states each of
//! its stages' gains in these units.
//!
//! Instruction and allocation counts, unlike wall time, barely move with
//! machine load, so they are what a claimed gain is measured in; the wall time
//! is printed for orientation only. Instructions come from the kernel's
//! per-process counter (`proc_pid_rusage`, macOS only; elsewhere the column
//! reads `n/a`). Allocations come from a counting wrapper around the system
//! allocator. Each phase reports the minimum over `PARSE_COUNTS_REPS` runs
//! (default 3).
//!
//! It lives in `borzoi-sema` rather than `borzoi-cst` because an example sees
//! its crate's dev-dependencies, and `borzoi-cst`'s turn on the test-only
//! `rowan-reference` feature, which runs a second tree builder inside every
//! parse.
//!
//! ```sh
//! nix develop -c cargo run --release -p borzoi-sema --example parse_counts
//! ```
//!
//! * `BORZOI_CORPUS` (set by `nix develop`): the corpus root.
//! * `PARSE_COUNTS_SUBDIR` (default `src`): the subtree of it to measure;
//!   `.` measures the whole corpus.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use borzoi_cst::directives::{PreprocError, TriviaToken, lex_with_symbols_full_trivia};
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::lexfilter::filter_collect;
use borzoi_cst::parser::parse_with_symbols;

/// The system allocator, counting calls and requested bytes.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: every method forwards to `System` with its arguments unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A reallocation is an allocation of the new block: counting it is what
        // makes a growing `Vec` visible.
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Instructions this process has retired so far, where the platform says.
#[cfg(target_os = "macos")]
fn instructions_retired() -> Option<u64> {
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
        fn getpid() -> i32;
    }
    /// `RUSAGE_INFO_V4`, whose struct is a 16-byte UUID followed by `u64`s.
    const RUSAGE_INFO_V4: i32 = 4;
    /// `ri_instructions`'s index among those `u64`s.
    const RI_INSTRUCTIONS: usize = 29;
    let mut buffer = [0u64; 64];
    // SAFETY: `buffer` is larger than `rusage_info_v4` (37 words).
    let rc = unsafe { proc_pid_rusage(getpid(), RUSAGE_INFO_V4, buffer.as_mut_ptr()) };
    (rc == 0).then(|| buffer[2 + RI_INSTRUCTIONS])
}

#[cfg(not(target_os = "macos"))]
fn instructions_retired() -> Option<u64> {
    None
}

/// What one run of a phase cost.
#[derive(Clone, Copy)]
struct Cost {
    wall: Duration,
    instructions: Option<u64>,
    allocations: u64,
    allocated_bytes: u64,
}

impl Cost {
    fn min(self, other: Cost) -> Cost {
        Cost {
            wall: self.wall.min(other.wall),
            instructions: match (self.instructions, other.instructions) {
                (Some(a), Some(b)) => Some(a.min(b)),
                _ => None,
            },
            allocations: self.allocations.min(other.allocations),
            allocated_bytes: self.allocated_bytes.min(other.allocated_bytes),
        }
    }
}

/// Run `f` `reps` times; print the cheapest run of each count per source byte;
/// return the last result, so the work is not optimised away and later phases
/// can consume it. The result is dropped outside the measured region.
fn phase<T>(label: &str, reps: usize, bytes: usize, mut f: impl FnMut() -> T) -> T {
    let mut best: Option<Cost> = None;
    let mut last = None;
    for _ in 0..reps {
        drop(last.take());
        let (a0, b0, i0) = (
            ALLOCATIONS.load(Relaxed),
            ALLOCATED_BYTES.load(Relaxed),
            instructions_retired(),
        );
        let start = Instant::now();
        let result = std::hint::black_box(f());
        let wall = start.elapsed();
        let i1 = instructions_retired();
        let cost = Cost {
            wall,
            instructions: i0.zip(i1).map(|(i0, i1)| i1 - i0),
            allocations: ALLOCATIONS.load(Relaxed) - a0,
            allocated_bytes: ALLOCATED_BYTES.load(Relaxed) - b0,
        };
        best = Some(best.map_or(cost, |b| b.min(cost)));
        last = Some(result);
    }
    let cost = best.expect("reps > 0");
    let per_byte = |n: u64| n as f64 / bytes as f64;
    let instructions = cost
        .instructions
        .map_or_else(|| "n/a".to_string(), |i| format!("{:.1}", per_byte(i)));
    println!(
        "{label:<12} {instructions:>9} instr/B {:>8.4} allocs/B {:>7.2} alloc-B/B {:>8.1} ns/B (wall)",
        per_byte(cost.allocations),
        per_byte(cost.allocated_bytes),
        cost.wall.as_secs_f64() * 1e9 / bytes as f64,
    );
    last.expect("reps > 0")
}

fn main() {
    let corpus = PathBuf::from(std::env::var_os("BORZOI_CORPUS").expect("BORZOI_CORPUS unset"));
    let subdir = std::env::var("PARSE_COUNTS_SUBDIR").unwrap_or_else(|_| "src".into());
    let reps: usize = std::env::var("PARSE_COUNTS_REPS")
        .ok()
        .map(|s| s.parse().expect("PARSE_COUNTS_REPS is a number"))
        .unwrap_or(3);
    assert!(reps > 0, "PARSE_COUNTS_REPS must be positive");

    let files = fsharp_files(&corpus.join(subdir));
    let bytes: usize = files.iter().map(String::len).sum();
    println!("{} .fs files, {bytes} bytes, min of {reps}", files.len());
    let symbols = HashSet::new();
    // `parse_with_symbols` parses at its default language version.
    let lang = LanguageVersion::Preview;

    // The phases below are the production pipeline's (`parser::parse_inner`),
    // each fed the previous phase's output so it measures only itself.
    let raw = phase("lex", reps, bytes, || {
        files
            .iter()
            .map(|s| lex_with_symbols_full_trivia(s, &symbols).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    });
    let tokens = raw.iter().map(Vec::len).sum::<usize>();
    phase("lex-filter", reps, bytes, || {
        files
            .iter()
            .zip(&raw)
            .map(|(s, raw)| {
                let active = raw.iter().filter_map(|(res, span)| match res {
                    Ok(TriviaToken::Lexed(t)) => Some((Ok(t.clone()), span.clone())),
                    Err(PreprocError::Lex(e)) => Some((Err(e.clone()), span.clone())),
                    _ => None,
                });
                filter_collect(s, lang, active)
            })
            .collect::<Vec<_>>()
    });
    drop(raw);
    phase("parse", reps, bytes, || {
        files
            .iter()
            .map(|s| parse_with_symbols(s, &symbols))
            .collect::<Vec<_>>()
    });
    println!(
        "{tokens} raw tokens ({:.3} per source byte)",
        tokens as f64 / bytes as f64
    );
}

fn fsharp_files(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "fs") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}
