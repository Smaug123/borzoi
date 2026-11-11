//! Wall-clock phase timings of the analysis pipeline over a corpus: parse,
//! two syntax-tree walks, `resolve_file` and `infer_file`, each file analysed
//! on its own against the BCL. Each phase prints its time, nanoseconds per
//! source byte and throughput; the syntax tables' heap size prints per source
//! byte too.
//!
//! A measurement, not a gate: shared machines make wall time noisy, so nothing
//! asserts on it. Each phase reports the minimum over `PHASE_TIMINGS_REPS`
//! runs (default 3). It is an example rather than a test because the test
//! binaries install `borzoi_oracle_harness::alloc_limit`'s counting allocator,
//! and this should time what the LSP runs, on the system allocator.
//!
//! ```sh
//! nix develop -c sh -c 'PHASE_TIMINGS_FSHARP_CORE=$PWD/tools/fcs-dump/bin/Release/net10.0/FSharp.Core.dll \
//!   cargo run --release -p borzoi-sema --example phase_timings'
//! ```
//!
//! * `BORZOI_CORPUS` (set by `nix develop`): the corpus root.
//! * `PHASE_TIMINGS_SUBDIR` (default `src`): the subtree of it to time; the
//!   compiler's own sources are large real files. `.` times the whole corpus.
//! * `PHASE_TIMINGS_FSHARP_CORE`: an `FSharp.Core.dll` (building
//!   `tools/fcs-dump` puts one beside it). `System.Runtime.dll` and
//!   `netstandard.dll` come from the reference pack under `DOTNET_ROOT`.
//!
//! To profile a phase, run the built binary under a sampling profiler (on
//! macOS, `xcrun xctrace record --template 'Time Profiler' --launch -- …`;
//! copy the binary out of `~/Documents` first, or macOS asks for folder
//! access).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse_with_symbols;
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxKind, SyntaxNode};
use borzoi_sema::{
    AssemblyEnv, PrecedingTypes, ProjectItems, SyntaxRecovery, infer_file, resolve_file,
};

fn main() {
    let corpus = PathBuf::from(std::env::var_os("BORZOI_CORPUS").expect("BORZOI_CORPUS unset"));
    let subdir = std::env::var("PHASE_TIMINGS_SUBDIR").unwrap_or_else(|_| "src".into());
    let reps: usize = std::env::var("PHASE_TIMINGS_REPS")
        .ok()
        .map(|s| s.parse().expect("PHASE_TIMINGS_REPS is a number"))
        .unwrap_or(3);
    assert!(reps > 0, "PHASE_TIMINGS_REPS must be positive");

    let files = fsharp_files(&corpus.join(subdir));
    let bytes: usize = files.iter().map(String::len).sum();
    println!("{} .fs files, {bytes} bytes, min of {reps}", files.len());
    let env = bcl_env();
    let symbols = HashSet::new();

    let parses = phase("parse", reps, bytes, || {
        files
            .iter()
            .map(|s| parse_with_symbols(s, &symbols))
            .collect::<Vec<_>>()
    });
    let nodes: usize = parses.iter().map(|p| p.root.descendants().count()).sum();
    let elements: usize = parses
        .iter()
        .map(|p| p.root.descendants_with_tokens().count())
        .sum();
    let table_bytes: usize = parses.iter().map(|p| p.root.table_heap_bytes()).sum();
    println!(
        "{nodes} nodes, {elements} nodes and tokens; syntax tables {:.1} bytes per source byte",
        table_bytes as f64 / bytes as f64
    );
    phase("walk: descendants_with_tokens", reps, bytes, || {
        parses.iter().map(|p| preorder_walk(&p.root)).sum::<u64>()
    });
    phase("walk: recursive children()", reps, bytes, || {
        parses.iter().map(|p| children_walk(&p.root)).sum::<u64>()
    });

    let impls: Vec<(ImplFile, SyntaxRecovery)> = parses
        .iter()
        .filter(|p| p.root.kind() == SyntaxKind::IMPL_FILE)
        .map(|p| {
            let file = ImplFile::cast(p.root.clone()).expect("an IMPL_FILE root casts");
            (file, SyntaxRecovery::of(p))
        })
        .collect();
    let resolved = phase("resolve_file", reps, bytes, || {
        impls
            .iter()
            .map(|(file, recovery)| resolve_file(file, &ProjectItems::default(), &env, recovery))
            .collect::<Vec<_>>()
    });
    phase("infer_file", reps, bytes, || {
        impls
            .iter()
            .zip(&resolved)
            .map(|((file, _), r)| infer_file(file, r, &env, &PrecedingTypes::default()))
            .collect::<Vec<_>>()
    });
}

/// Run `f` `reps` times, print the fastest (as time, per byte of the
/// `bytes` of source, and as throughput), and return the last result (so the
/// work is not optimised away, and later phases can consume it).
fn phase<T>(label: &str, reps: usize, bytes: usize, mut f: impl FnMut() -> T) -> T {
    let mut best = Duration::MAX;
    let mut last = None;
    for _ in 0..reps {
        let start = Instant::now();
        let result = std::hint::black_box(f());
        best = best.min(start.elapsed());
        last = Some(result);
    }
    let secs = best.as_secs_f64();
    println!(
        "{label:<32} {:>9.1} ms {:>8.1} ns/byte {:>8.1} MB/s",
        secs * 1e3,
        secs * 1e9 / bytes as f64,
        bytes as f64 / secs / 1e6
    );
    last.expect("reps > 0")
}

/// Every element in preorder, touching its kind and range.
fn preorder_walk(root: &SyntaxNode) -> u64 {
    root.descendants_with_tokens().fold(0u64, |acc, el| {
        acc.wrapping_add(el.kind() as u64)
            .wrapping_add(u64::from(u32::from(el.text_range().start())))
    })
}

/// Child-by-child navigation, the shape every typed accessor walks.
/// Recursion depth is bounded by the parser's `MAX_TREE_HEIGHT`; the main
/// thread's stack holds it for real source.
fn children_walk(node: &SyntaxNode) -> u64 {
    node.children().fold(node.kind() as u64, |acc, c| {
        acc.wrapping_add(children_walk(&c))
    })
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

/// `FSharp.Core` plus `System.Runtime` and the `netstandard` facade, the set
/// the sema tests' `full_bcl_env` uses.
fn bcl_env() -> AssemblyEnv {
    let core_path = std::env::var_os("PHASE_TIMINGS_FSHARP_CORE")
        .expect("PHASE_TIMINGS_FSHARP_CORE unset: point it at an FSharp.Core.dll");
    let packs = PathBuf::from(std::env::var_os("DOTNET_ROOT").expect("DOTNET_ROOT unset"))
        .join("packs")
        .join("Microsoft.NETCore.App.Ref");
    let ref_dir = std::fs::read_dir(&packs)
        .unwrap_or_else(|e| panic!("{}: {e}", packs.display()))
        .map(|e| e.expect("pack entry").path().join("ref").join("net10.0"))
        .find(|p| p.join("System.Runtime.dll").exists())
        .expect("no ref/net10.0/System.Runtime.dll in the reference pack");
    let read = |p: &Path| std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let images = [
        read(Path::new(&core_path)),
        read(&ref_dir.join("System.Runtime.dll")),
        read(&ref_dir.join("netstandard.dll")),
    ];
    let views: Vec<_> = images
        .iter()
        .map(|bytes| Ecma335Assembly::parse(bytes).expect("parse a BCL assembly"))
        .collect();
    AssemblyEnv::from_views(&views).expect("build the AssemblyEnv")
}
